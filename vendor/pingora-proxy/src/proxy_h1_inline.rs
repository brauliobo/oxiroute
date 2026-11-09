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

use super::*;
use crate::proxy_cache::{range_filter::RangeBodyFilter, ServeFromCache};
use crate::proxy_h1::send_body_to1;

/// What the loop waits for next.
enum Event {
    Response(Result<HttpTask>),
    Request(Result<Option<Bytes>>),
}

/// The progress of one exchange.
struct Exchange {
    /// The client sent all of its request body (or its end was seen), so only a close is waited for.
    reading_done: bool,
    /// The request body reached the end on the upstream, or the upstream failed to take more of it. It implies
    /// `reading_done`: a client that goes on uploading after the upstream gave up on the request is not drained, its
    /// extra data ends the exchange (the idle read of a finished body fails on data).
    request_done: bool,
    /// The upstream response ended (or an upgraded connection ended on the client side).
    response_done: bool,
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
            Ok(reuse_downstream) => (reuse_downstream, true, None),
            Err(error) => (false, false, Some(error)),
        }
    }

    async fn relay_inline(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        ctx: &mut SV::CTX,
    ) -> Result<bool>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut exchange = Exchange {
            reading_done: session.as_mut().is_body_done(),
            request_done: false,
            response_done: false,
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
        while !exchange.reading_done || !exchange.response_done {
            match Self::next_event(session, client_session, &exchange).await {
                Event::Response(first) => {
                    self.relay_response(
                        session,
                        client_session,
                        &mut exchange,
                        first,
                        ctx,
                        &mut serve_from_cache,
                        &mut range_body_filter,
                    )
                    .await?
                }
                Event::Request(body) => {
                    let body = body.map_err(|e| e.into_down())?;
                    // Once the body is read the wait is only for the close of the connection, which is an error.
                    if exchange.reading_done {
                        continue;
                    }
                    // The end of an upgraded connection on the client side ends the response as well.
                    if body.is_none() && session.was_upgraded() {
                        exchange.response_done = true;
                    }
                    let end = session.is_body_done();
                    exchange.reading_done = end || body.is_none();
                    self.forward_request_body(
                        session,
                        client_session,
                        &mut exchange,
                        body,
                        end,
                        ctx,
                    )
                    .await?;
                }
            }
        }
        // The client is done with its body: end the request on the upstream if that has not happened yet.
        if !exchange.request_done {
            send_body_to1(client_session, None).await?;
        }
        match session.as_mut().finish_body().await {
            Ok(_) => {
                debug!("finished sending body to downstream");
                Ok(true)
            }
            Err(e) => {
                error!("Error finish sending body to downstream: {}", e);
                Ok(false)
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
                // The upstream takes no more of the request: stop reading the client and wait for the answer.
                exchange.request_done = true;
                exchange.reading_done = true;
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
            session.upstream_compression.response_filter(&mut task);
            let task = self
                .h1_response_filter(
                    session,
                    task,
                    ctx,
                    serve_from_cache,
                    range_body_filter,
                    false,
                )
                .await?;
            filtered_tasks.push(task);
        }
        let upgraded = session.was_upgraded();
        let response_done = session.write_response_task_batch(filtered_tasks).await?;
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
        Ok(())
    }

    fn failed(exchange: &mut Exchange, e: Box<Error>) -> HttpTask {
        HttpTask::Failed(exchange.send_error.take().unwrap_or(e).into_up())
    }
}
