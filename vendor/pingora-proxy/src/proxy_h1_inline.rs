//! The HTTP/1 relay that runs both directions of a request in one loop.
//!
//! [`HttpProxy::proxy_1to1`] runs the two directions of a request as two futures that talk through two channels,
//! because a request body and a response body can flow at the same time. One task can do the same: it waits for
//! either the next response task of the upstream or the next request body chunk of the client, forwards what arrived
//! and goes back to waiting. A request body is written to the upstream right where it is read and a response task
//! is written to the client right where it is read, through the same filters, batches and error mapping as the two
//! futures use, without the channels, the permit bookkeeping and the second future to poll. The upstream half
//! cannot run ahead of a blocked client write by up to a channel of tasks any more; neither half was ever able to
//! make progress while the other one was blocked in a write, so nothing that terminated before waits longer now.
//!
//! The loop covers requests with and without a body, upgrades and replayed request bodies, for HTTP/1 and HTTP/2
//! clients. A request that involves the cache or a custom (HTTP/3) client keeps the two futures.

use bytes::Bytes;

use pingora_core::apps::PooledFuture;

use super::*;
use crate::proxy_cache::{range_filter::RangeBodyFilter, ServeFromCache};
use crate::proxy_h1::send_body_to1;
use crate::proxy_tunnel::TunnelState;

/// What the loop waits for next.
enum Event {
    Response(Result<HttpTask>),
    Request(Result<Option<Bytes>>),
}

/// How the relay loop ended.
#[derive(PartialEq, Eq)]
pub(crate) enum LoopEnd {
    /// The exchange is over (or failed): the request is finished.
    Done,
    /// The upgrade handshake is complete and the connection goes on as a tunnel, outside this request's state machine.
    Upgraded,
}

/// The progress of one exchange.
pub(crate) struct Exchange {
    /// The client sent all of its request body (or its end was seen), so only a close is waited for: data after it
    /// ends the exchange (the idle read of a finished body fails on data).
    reading_done: bool,
    /// The request body reached the end on the upstream, or the upstream failed to take more of it. A client that
    /// goes on uploading after the upstream gave up on the request is read and dropped until the answer is relayed:
    /// the answer is what the client is owed, and the upstream may not have shown it yet.
    request_done: bool,
    /// The upstream response ended (or an upgraded connection ended on the client side).
    response_done: bool,
    /// The upstream response was read to its end. A response the client finished first leaves bytes on the upstream
    /// connection.
    response_read: bool,
    /// The request was written to the upstream in full. A failed write leaves a partial request on the connection.
    request_written: bool,
    /// The first error of writing the request to the upstream; it explains a later failure to read the response.
    send_error: Option<Box<Error>>,
}

impl<SV, C> HttpProxy<SV, C>
where
    C: custom::Connector,
{
    /// Whether the exchange can use [`Self::proxy_1to1_inline`]: an HTTP/1 or HTTP/2 client and no cached response.
    pub(crate) fn relays_inline(session: &mut Session) -> bool {
        (session.downstream_session.as_http1().is_some()
            || session.downstream_session.as_http2().is_some())
            && !session.cache.enabled()
            && !session.cache.bypassing()
    }

    /// Same result as [`Self::proxy_1to1`] for a request that passed [`Self::relays_inline`], once the request
    /// header is written upstream.
    pub(crate) async fn proxy_1to1_inline(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        ctx: &mut SV::CTX,
    ) -> (bool, bool, Option<Box<Error>>)
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        session.as_mut().enable_retry_buffering();
        match self.relay_inline(session, client_session, ctx).await {
            Ok((reuse_downstream, reuse_upstream)) => (reuse_downstream, reuse_upstream, None),
            Err(error) => (false, false, Some(error)),
        }
    }

    async fn relay_inline(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        ctx: &mut SV::CTX,
    ) -> Result<(bool, bool)>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut exchange = Exchange {
            reading_done: session.as_mut().is_body_done(),
            request_done: false,
            response_done: false,
            response_read: false,
            request_written: true,
            send_error: None,
        };
        // A retried request sends what the former attempt read, and a request without a body sends its end.
        let buffer = session.as_ref().get_retry_buffer();
        if buffer.is_some() || session.as_mut().is_body_empty() {
            let end = exchange.reading_done;
            self.forward_request_body(session, client_session, &mut exchange, buffer, end, ctx)
                .await?;
        }

        let mut serve_from_cache = ServeFromCache::new();
        let mut range_body_filter = RangeBodyFilter::new();
        let end = self
            .relay_loop(
                session,
                client_session,
                &mut exchange,
                ctx,
                &mut serve_from_cache,
                &mut range_body_filter,
                true,
            )
            .await?;
        if end == LoopEnd::Upgraded {
            session.tunnel = Some(Box::new(TunnelState::new(exchange)));
            return Ok((false, false));
        }
        self.relay_end(session, client_session, exchange).await
    }

    /// Continues the exchange of an upgraded connection to its end, outside the request's state machine (see
    /// [`crate::proxy_tunnel`]). Same result as [`Self::proxy_1to1_inline`].
    pub(crate) async fn relay_resumed(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        mut exchange: Exchange,
        ctx: &mut SV::CTX,
    ) -> (bool, bool, Option<Box<Error>>)
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut serve_from_cache = ServeFromCache::new();
        let mut range_body_filter = RangeBodyFilter::new();
        let relayed = match self
            .relay_loop(
                session,
                client_session,
                &mut exchange,
                ctx,
                &mut serve_from_cache,
                &mut range_body_filter,
                false,
            )
            .await
        {
            Ok(_) => self.relay_end(session, client_session, exchange).await,
            Err(error) => Err(error),
        };
        match relayed {
            Ok((reuse_downstream, reuse_upstream)) => (reuse_downstream, reuse_upstream, None),
            Err(error) => (false, false, Some(error)),
        }
    }

    /// Relays until both directions are done. With `may_tunnel`, returns as soon as the upgrade handshake is complete
    /// and the exchange can continue as a tunnel.
    #[allow(clippy::too_many_arguments)]
    async fn relay_loop(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        exchange: &mut Exchange,
        ctx: &mut SV::CTX,
        serve_from_cache: &mut ServeFromCache,
        range_body_filter: &mut RangeBodyFilter,
        may_tunnel: bool,
    ) -> Result<LoopEnd>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        while !(exchange.reading_done || exchange.request_done) || !exchange.response_done {
            match Self::next_event(session, client_session, exchange).await {
                Event::Response(first) => {
                    let upgrading = may_tunnel && !session.was_upgraded();
                    self.relay_response(
                        session,
                        client_session,
                        exchange,
                        first,
                        ctx,
                        serve_from_cache,
                        range_body_filter,
                    )
                    .await?;
                    if upgrading && Self::can_tunnel(session, client_session, exchange) {
                        return Ok(LoopEnd::Upgraded);
                    }
                }
                Event::Request(body) => {
                    let body = body.map_err(|e| e.into_down())?;
                    // Once the body is read the wait is only for the close of the connection, which is an error.
                    if exchange.reading_done {
                        continue;
                    }
                    if exchange.request_done {
                        exchange.reading_done = body.is_none() || session.is_body_done();
                        continue;
                    }
                    // The end of an upgraded connection on the client side ends the response as well.
                    if body.is_none() && session.was_upgraded() {
                        exchange.response_done = true;
                    }
                    let end = session.is_body_done();
                    exchange.reading_done = end || body.is_none();
                    self.forward_request_body(session, client_session, exchange, body, end, ctx)
                        .await?;
                }
            }
        }
        Ok(LoopEnd::Done)
    }

    /// Whether the connection can go on as a tunnel: an HTTP/1 client and upstream that both completed the upgrade
    /// and nothing of the exchange is over or failed.
    fn can_tunnel(
        session: &mut Session,
        client_session: &HttpSessionV1,
        exchange: &Exchange,
    ) -> bool {
        session.subrequest_ctx.is_none()
            && session.downstream_session.as_http1().is_some()
            && session.was_upgraded()
            && client_session.was_upgraded()
            && exchange.send_error.is_none()
            && !(exchange.reading_done || exchange.request_done || exchange.response_done)
    }

    /// The end of an exchange: the request is ended on the upstream if that has not happened yet, and the response
    /// to the client is finished.
    async fn relay_end(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        exchange: Exchange,
    ) -> Result<(bool, bool)> {
        // The client is done with its body: end the request on the upstream if that has not happened yet.
        if !exchange.request_done {
            send_body_to1(client_session, None).await?;
        }
        let reuse_upstream = exchange.response_read && exchange.request_written;
        match session.as_mut().finish_body().await {
            Ok(_) => {
                debug!("finished sending body to downstream");
                Ok((true, reuse_upstream))
            }
            Err(e) => {
                error!("Error finish sending body to downstream: {}", e);
                Ok((false, reuse_upstream))
            }
        }
    }

    /// Waits for the next response task of the upstream or request body chunk of the client. Once the client is done
    /// with its body, the second wait only detects the client closing the connection, and the response is preferred.
    async fn next_event(
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        exchange: &Exchange,
    ) -> Event {
        let response_pending = !exchange.response_done;
        if exchange.reading_done {
            tokio::select! {
                biased;
                task = client_session.read_response_task(), if response_pending => Event::Response(task),
                idle = session.downstream_session.read_body_or_idle(true) => Event::Request(idle),
            }
        } else {
            tokio::select! {
                task = client_session.read_response_task(), if response_pending => Event::Response(task),
                body = session.downstream_session.read_body_or_idle(false) => Event::Request(body),
            }
        }
    }

    /// Runs the request body filters on a chunk read from the client and writes it to the upstream.
    async fn forward_request_body(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        exchange: &mut Exchange,
        mut data: Option<Bytes>,
        end_of_body: bool,
        ctx: &mut SV::CTX,
    ) -> Result<()>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let end_of_body = end_of_body || data.is_none();
        session
            .downstream_modules_ctx
            .request_body_filter(&mut data, end_of_body)
            .await?;
        self.inner
            .request_body_filter(session, &mut data, end_of_body, ctx)
            .await?;
        let upstream_end_of_body = end_of_body || data.is_none();
        // A filter can hold a chunk back; an empty chunk must not reach the wire as the terminating chunk.
        if !upstream_end_of_body && data.as_ref().is_some_and(|d| d.is_empty()) {
            return Ok(());
        }
        debug!(
            "Read {} bytes body from downstream",
            data.as_ref().map_or(-1, |d| d.len() as isize)
        );
        let task = if session.was_upgraded() {
            HttpTask::UpgradedBody(data, upstream_end_of_body)
        } else {
            HttpTask::Body(data, upstream_end_of_body)
        };
        match send_body_to1(client_session, Some(task)).await {
            Ok(done) => {
                // What the client still sends after the upstream saw the end of the request is not forwarded.
                exchange.request_done = done;
                exchange.reading_done |= done;
                // An upgraded connection ends when either side is done.
                if done && client_session.was_upgraded() {
                    exchange.response_done = true;
                }
            }
            Err(e) => {
                warn!("send error, draining read buf: {e}");
                // The upstream takes no more of the request: what the client still sends is dropped, and the answer
                // is waited for.
                exchange.request_done = true;
                exchange.reading_done = session.is_body_done();
                exchange.request_written = false;
                exchange.send_error = Some(e);
            }
        }
        Ok(())
    }

    /// Reads the tasks the upstream session already holds after `first`, filters them and writes them to the client
    /// in one batch.
    #[allow(clippy::too_many_arguments)]
    async fn relay_response(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        exchange: &mut Exchange,
        first: Result<HttpTask>,
        ctx: &mut SV::CTX,
        serve_from_cache: &mut ServeFromCache,
        range_body_filter: &mut RangeBodyFilter,
    ) -> Result<()>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut tasks = HttpTaskBatch::new();
        let mut ended = match first {
            Ok(task) => {
                let ended = task.is_end();
                tasks.push(task);
                ended
            }
            Err(e) => {
                tasks.push(Self::failed(exchange, e));
                true
            }
        };
        // Whatever the upstream session already holds, or reads at once, goes out in the same write.
        while !ended && !tasks.spilled() && tasks.len() < TASK_BUFFER_SIZE {
            match client_session.try_read_response_task() {
                Some(Ok(task)) => {
                    ended = task.is_end();
                    tasks.push(task);
                }
                Some(Err(e)) => {
                    tasks.push(Self::failed(exchange, e));
                    ended = true;
                }
                None => break,
            }
        }

        let mut filtered_tasks = HttpTaskBatch::new();
        for mut task in tasks {
            #[cfg(feature = "upstream_modules")]
            if let HttpTask::Header(header, end_of_stream) = &task {
                self.inner
                    .adjust_upstream_modules(session, header, *end_of_stream, ctx)
                    .await?;
            }
            #[cfg(feature = "upstream_modules")]
            session.upstream_modules_filter_task(&mut task).await?;
            session.upstream_compression.response_filter(&mut task);
            // On the heap: these futures are 2 to 4 KiB each and would be part of the relay's state for the whole
            // life of an exchange, tunnels included.
            let task = {
                let (session, ctx) = (&mut *session, &mut *ctx);
                let (serve_from_cache, range_body_filter) =
                    (&mut *serve_from_cache, &mut *range_body_filter);
                PooledFuture::new(move || {
                    self.h1_response_filter(
                        session,
                        task,
                        ctx,
                        serve_from_cache,
                        range_body_filter,
                        false,
                    )
                })
                .await?
            };
            filtered_tasks.push(task);
        }
        let upgraded = session.was_upgraded();
        let response_done = {
            let session = &mut *session;
            PooledFuture::new(move || session.write_response_task_batch(filtered_tasks)).await?
        };
        if !upgraded && session.was_upgraded() {
            // Just upgraded: the client body reader now passes bytes as they are, and they go to the upstream.
            if exchange.send_error.is_none() {
                exchange.reading_done = false;
                exchange.request_done = false;
            }
        }
        // An unsuccessful upgrade response, or the end of an upgraded connection, can complete the request body.
        exchange.reading_done |= session.is_body_done();
        exchange.response_done |= response_done || ended;
        exchange.response_read |= ended;
        Ok(())
    }

    fn failed(exchange: &mut Exchange, e: Box<Error>) -> HttpTask {
        HttpTask::Failed(exchange.send_error.take().unwrap_or(e).into_up())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    struct Relay;

    impl ProxyHttp for Relay {
        type CTX = ();

        fn new_ctx(&self) -> Self::CTX {}

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unreachable!("the relay tests do not select an upstream")
        }
    }

    /// Relays one request to an origin that answers it in full and goes away before the client sends `body`.
    async fn relay(
        request: &'static [u8],
        body: &'static [u8],
    ) -> (bool, bool, Option<Box<Error>>) {
        let (mut client, server) = tokio::io::duplex(8192);
        client.write_all(request).await.unwrap();
        let mut session = Session::new_h1_with_modules(Box::new(server), &HttpModules::new());
        session.as_downstream_mut().read_request().await.unwrap();

        let (proxy_io, mut origin_io) = tokio::io::duplex(8192);
        let origin = tokio::spawn(async move {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(origin_io.read_u8().await.unwrap());
            }
            origin_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let mut upstream = HttpSessionV1::new(Box::new(proxy_io));
        upstream
            .write_request_header_ref(session.req_header())
            .await
            .unwrap();
        origin.await.unwrap();
        client.write_all(body).await.unwrap();

        let proxy = HttpProxy::new(Relay, Arc::new(ServerConf::default()));
        proxy
            .proxy_1to1_inline(&mut session, &mut upstream, &mut ())
            .await
    }

    #[tokio::test]
    async fn a_complete_exchange_leaves_the_upstream_reusable() {
        let (_, reuse_upstream, error) =
            relay(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n", b"").await;

        assert!(error.is_none(), "{error:?}");
        assert!(reuse_upstream);
    }

    #[tokio::test]
    async fn a_failed_request_write_leaves_the_upstream_unreusable() {
        let (_, reuse_upstream, error) = relay(
            b"POST / HTTP/1.1\r\nHost: example.test\r\nContent-Length: 4\r\n\r\n",
            b"abcd",
        )
        .await;

        assert!(error.is_none(), "{error:?}");
        assert!(!reuse_upstream);
    }

    /// An upstream connection that takes the request head, refuses the body and holds its answer back until the test
    /// lets it through: the proxy has seen the refusal before it can see the answer.
    #[derive(Debug)]
    struct Refusing {
        writes: usize,
        answered: bool,
        gate: Arc<std::sync::Mutex<(bool, Option<std::task::Waker>)>>,
        refused: Arc<tokio::sync::Notify>,
    }

    impl Refusing {
        const ANSWER: &'static [u8] =
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

        fn open(gate: &std::sync::Mutex<(bool, Option<std::task::Waker>)>) {
            let mut gate = gate.lock().unwrap();
            gate.0 = true;
            if let Some(waker) = gate.1.take() {
                waker.wake();
            }
        }
    }

    impl tokio::io::AsyncRead for Refusing {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let gate = Arc::clone(&self.gate);
            let mut gate = gate.lock().unwrap();
            if !gate.0 {
                gate.1 = Some(cx.waker().clone());
                return std::task::Poll::Pending;
            }
            if !std::mem::replace(&mut self.answered, true) {
                buf.put_slice(Self::ANSWER);
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl tokio::io::AsyncWrite for Refusing {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.writes += 1;
            if self.writes == 1 {
                return std::task::Poll::Ready(Ok(buf.len()));
            }
            self.refused.notify_one();
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[async_trait::async_trait]
    impl pingora_core::protocols::Shutdown for Refusing {
        async fn shutdown(&mut self) {}
    }

    impl pingora_core::protocols::UniqueID for Refusing {
        fn id(&self) -> pingora_core::protocols::UniqueIDType {
            0
        }
    }

    impl pingora_core::protocols::Ssl for Refusing {}

    impl pingora_core::protocols::GetTimingDigest for Refusing {
        fn get_timing_digest(&self) -> Vec<Option<pingora_core::protocols::TimingDigest>> {
            vec![]
        }
    }

    impl pingora_core::protocols::GetProxyDigest for Refusing {
        fn get_proxy_digest(
            &self,
        ) -> Option<Arc<pingora_core::protocols::raw_connect::ProxyDigest>> {
            None
        }
    }

    impl pingora_core::protocols::GetSocketDigest for Refusing {
        fn get_socket_digest(&self) -> Option<Arc<pingora_core::protocols::SocketDigest>> {
            None
        }
    }

    impl pingora_core::protocols::Peek for Refusing {}

    /// The origin refused the body before its answer could be seen, and the client goes on uploading: the data is
    /// dropped and the answer, once it is there, reaches the client.
    #[tokio::test]
    async fn an_upload_that_goes_on_after_the_upstream_refused_it_still_gets_the_answer() {
        let (mut client, server) = tokio::io::duplex(65536);
        client
            .write_all(b"POST / HTTP/1.1\r\nHost: example.test\r\nContent-Length: 6000\r\n\r\n")
            .await
            .unwrap();
        let mut session = Session::new_h1_with_modules(Box::new(server), &HttpModules::new());
        session.as_downstream_mut().read_request().await.unwrap();
        let gate = Arc::new(std::sync::Mutex::new((false, None)));
        let refused = Arc::new(tokio::sync::Notify::new());
        let mut upstream = HttpSessionV1::new(Box::new(Refusing {
            writes: 0,
            answered: false,
            gate: Arc::clone(&gate),
            refused: Arc::clone(&refused),
        }));
        upstream
            .write_request_header_ref(session.req_header())
            .await
            .unwrap();

        let proxy = HttpProxy::new(Relay, Arc::new(ServerConf::default()));
        let mut ctx = ();
        let relayed = proxy.proxy_1to1_inline(&mut session, &mut upstream, &mut ctx);
        let upload = async {
            client.write_all(&[0; 1000]).await.unwrap();
            refused.notified().await;
            client.write_all(&[0; 1000]).await.unwrap();
            // The relay has taken the data, or ended on it, by the time the answer can be seen.
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            Refusing::open(&gate);
        };
        let ((_, reuse_upstream, error), ()) = tokio::join!(relayed, upload);

        assert!(error.is_none(), "{error:?}");
        assert!(!reuse_upstream);
        drop(session);
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).await.unwrap();
        assert!(answer.starts_with(b"HTTP/1.1 200 OK"), "{answer:?}");
    }

    /// An origin that accepts the upgrade and then echoes what it is sent until the proxy closes the connection.
    async fn echoing_origin(mut origin_io: tokio::io::DuplexStream) {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(origin_io.read_u8().await.unwrap());
        }
        origin_io
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            )
            .await
            .unwrap();
        let mut chunk = [0_u8; 64];
        loop {
            match origin_io.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => origin_io.write_all(&chunk[..count]).await.unwrap(),
            }
        }
    }

    /// The relay hands the connection over as soon as the handshake is complete, and the tunnel relays both ways.
    #[tokio::test]
    async fn an_upgrade_continues_as_a_tunnel_once_the_handshake_is_complete() {
        let (mut client, server) = tokio::io::duplex(8192);
        client
            .write_all(
                b"GET /socket HTTP/1.1\r\nHost: example.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            )
            .await
            .unwrap();
        let mut session = Session::new_h1_with_modules(Box::new(server), &HttpModules::new());
        session.as_downstream_mut().read_request().await.unwrap();
        let (proxy_io, origin_io) = tokio::io::duplex(8192);
        tokio::spawn(echoing_origin(origin_io));
        let mut upstream = HttpSessionV1::new(Box::new(proxy_io));
        upstream
            .write_request_header_ref(session.req_header())
            .await
            .unwrap();

        let proxy = HttpProxy::new(Relay, Arc::new(ServerConf::default()));
        let (reuse_downstream, reuse_upstream, error) = proxy
            .proxy_1to1_inline(&mut session, &mut upstream, &mut ())
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(!reuse_downstream && !reuse_upstream);
        let exchange = session
            .tunnel
            .take()
            .expect("the upgrade left a tunnel")
            .into_exchange();

        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(client.read_u8().await.unwrap());
        }
        assert!(head.starts_with(b"HTTP/1.1 101"), "{head:?}");

        let mut ctx = ();
        let resumed = proxy.relay_resumed(&mut session, &mut upstream, exchange, &mut ctx);
        let talk = async {
            client.write_all(b"hello").await.unwrap();
            let mut echoed = [0_u8; 5];
            client.read_exact(&mut echoed).await.unwrap();
            assert_eq!(&echoed, b"hello");
            drop(client);
        };
        let ((_, _, error), ()) = tokio::join!(resumed, talk);
        assert!(error.is_none(), "{error:?}");
    }

    /// A request that is not an upgrade never leaves a tunnel behind.
    #[tokio::test]
    async fn a_request_that_is_not_an_upgrade_leaves_no_tunnel() {
        let (mut client, server) = tokio::io::duplex(8192);
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n")
            .await
            .unwrap();
        let mut session = Session::new_h1_with_modules(Box::new(server), &HttpModules::new());
        session.as_downstream_mut().read_request().await.unwrap();
        let (proxy_io, mut origin_io) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(origin_io.read_u8().await.unwrap());
            }
            origin_io
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let mut upstream = HttpSessionV1::new(Box::new(proxy_io));
        upstream
            .write_request_header_ref(session.req_header())
            .await
            .unwrap();
        let proxy = HttpProxy::new(Relay, Arc::new(ServerConf::default()));
        let (_, _, error) = proxy
            .proxy_1to1_inline(&mut session, &mut upstream, &mut ())
            .await;
        assert!(error.is_none(), "{error:?}");
        assert!(session.tunnel.is_none());
    }
}
