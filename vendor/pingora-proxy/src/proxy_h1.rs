// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use futures::future::OptionFuture;
use futures::StreamExt;

use super::*;
use crate::proxy_cache::{range_filter::RangeBodyFilter, ServeFromCache};
use crate::proxy_common::*;
use crate::spsc;
use pingora_cache::CachePhase;
use pingora_core::protocols::http::{
    authority::{raw_target_authority, validate_request_authority},
    custom::CUSTOM_MESSAGE_QUEUE_SIZE,
    v1::common::{is_upgrade_req as is_h1_upgrade_req, RequestFacts},
};
use pingora_core::upstreams::peer::HttpUpstreamRequestPolicy;

fn collect_available_tasks(
    first: HttpTask,
    rx: &mut spsc::Receiver<'_, HttpTask, TASK_BUFFER_SIZE>,
) -> HttpTaskBatch {
    let mut tasks = HttpTaskBatch::new();
    tasks.push(first);
    while let Ok(task) = rx.try_recv() {
        debug!("upstream event now: {:?}", task);
        tasks.push(task);
    }
    tasks
}

fn cache_task_batch(task: HttpTask) -> HttpTaskBatch {
    let mut tasks = HttpTaskBatch::new();
    tasks.push(task);
    tasks
}

/// Prepares the request for a plain HTTP/1 upstream attempt.
///
/// An H2 client or an active cache needs its own copy of the request. Any other request is borrowed when the
/// application leaves it unchanged and the peer keeps the downstream fields (`preserve()`); a peer with another
/// policy gets a copy that the policy edits.
async fn prepare_h1_upstream_request<SV>(
    inner: &SV,
    session: &mut Session,
    ctx: &mut SV::CTX,
    policy: HttpUpstreamRequestPolicy,
) -> Result<PreparedUpstreamRequest>
where
    SV: ProxyHttp + Send + Sync,
    SV::CTX: Send + Sync,
{
    if session.req_header().version == Version::HTTP_2 || session.cache.enabled() {
        return prepare_converted_h1_upstream_request(inner, session, ctx, policy).await;
    }

    let prepared = inner.prepare_upstream_request(session, ctx).await?;
    if policy == HttpUpstreamRequestPolicy::preserve() {
        return Ok(prepared);
    }

    let mut req = match prepared {
        PreparedUpstreamRequest::Borrowed => Box::new(session.req_header().clone()),
        PreparedUpstreamRequest::Owned(req) => req,
    };
    let downstream_is_http11 = session.req_header().version == Version::HTTP_11;
    sanitize_h1_upstream_request(&mut req, policy, downstream_is_http11)
        .map_err(|e| e.into_down())?;
    finish_h1_upstream_request(session, &mut req)?;
    Ok(PreparedUpstreamRequest::Owned(req))
}

async fn prepare_converted_h1_upstream_request<SV>(
    inner: &SV,
    session: &mut Session,
    ctx: &mut SV::CTX,
    policy: HttpUpstreamRequestPolicy,
) -> Result<PreparedUpstreamRequest>
where
    SV: ProxyHttp + Send + Sync,
    SV::CTX: Send + Sync,
{
    let mut req = session.req_header().clone();
    let authority_policy = AuthorityPolicy::from(session.downstream_session.is_custom());
    let downstream_version = session.req_header().version;
    req.set_version(Version::HTTP_11);

    // H2 requires :authority but not Host; most H1 servers expect Host, so synthesize it when
    // sending an H2 downstream request to an H1 upstream.
    if downstream_version == Version::HTTP_2 {
        match authority_policy {
            AuthorityPolicy::Standard => set_h1_host_from_authority(&mut req),
            AuthorityPolicy::Custom => set_h1_host_from_authority_if_absent(&mut req),
        }
        .map_err(|e| e.into_down())?;
    }

    sanitize_h1_upstream_request(&mut req, policy, downstream_version == Version::HTTP_11)
        .map_err(|e| e.into_down())?;

    if session.cache.enabled() {
        pingora_cache::filters::upstream::request_filter(
            &mut req,
            session.cache.maybe_cache_meta(),
        );
        session.mark_upstream_headers_mutated_for_cache();
    }

    inner
        .upstream_request_filter(session, &mut req, ctx)
        .await?;
    finish_h1_upstream_request(session, &mut req)?;
    Ok(PreparedUpstreamRequest::Owned(Box::new(req)))
}

/// Reconciles and revalidates the authority and framing of a request the application could have changed.
fn finish_h1_upstream_request(session: &mut Session, req: &mut RequestHeader) -> Result<()> {
    if AuthorityPolicy::from(session.downstream_session.is_custom()).is_standard() {
        reconcile_upstream_authority(req).map_err(|e| e.into_in())?;
        // A filter may delete Host. Restore it for requests that originally carried one or
        // whose URI or absolute-form target has an authority, without adding a new empty Host
        // to Host-less HTTP/1.0 origin-form.
        let downstream_had_host = session
            .req_header()
            .headers
            .contains_key(http::header::HOST);
        let target_has_authority = raw_target_authority(req.raw_path()).authority().is_some();
        if downstream_had_host || req.uri.authority().is_some() || target_has_authority {
            set_h1_host_from_authority(req).map_err(|e| e.into_in())?;
        }
        // The final filter-produced request is invalid, so classify this as internal.
        validate_request_authority(req).map_err(|e| e.into_in())?;
    }

    finalize_h1_upstream_request_framing(req, !session.is_body_empty())?;
    session.set_upstream_h1_upgrade_request_status(is_h1_upgrade_req(req));
    Ok(())
}

impl<SV, C> HttpProxy<SV, C>
where
    C: custom::Connector,
{
    pub(crate) async fn proxy_1to1(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        peer: &HttpPeer,
        ctx: &mut SV::CTX,
    ) -> (bool, bool, Option<Box<Error>>)
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        client_session.read_timeout = peer.options.read_timeout;
        client_session.body_read_timeout = peer.options.body_read_timeout;
        client_session.write_timeout = peer.options.write_timeout;

        // phase 2 send to upstream

        let policy = peer.options.http_upstream_request_policy;
        let prepared = match prepare_h1_upstream_request(&self.inner, session, ctx, policy).await {
            Ok(prepared) => prepared,
            Err(e) => return (false, true, Some(e)),
        };

        // The downstream request went upstream as it is, so what it said about its framing still holds.
        let (req, facts) = match &prepared {
            PreparedUpstreamRequest::Borrowed => (
                session.downstream_session.req_header(),
                session.downstream_session.request_facts(),
            ),
            PreparedUpstreamRequest::Owned(req) => (&**req, RequestFacts::of_framing(&req.headers)),
        };
        session.upstream_compression.request_filter(req);

        debug!("Sending header to upstream {:?}", req);

        match client_session.write_request_header_noted(req, facts).await {
            Ok(_) => { /* Continue */ }
            Err(e) => {
                return (false, false, Some(e.into_up()));
            }
        }

        if Self::relays_inline(session) {
            return self.proxy_1to1_inline(session, client_session, ctx).await;
        }

        // Boxed: the request future would otherwise keep the state of this rarely taken path in every request.
        Box::pin(self.proxy_1to1_pumped(session, client_session, ctx)).await
    }

    /// The part of [`Self::proxy_1to1`] for a request that is not relayed inline: two task pumps joined.
    async fn proxy_1to1_pumped(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        ctx: &mut SV::CTX,
    ) -> (bool, bool, Option<Box<Error>>)
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut downstream_custom_message_writer = session
            .downstream_session
            .as_custom_mut()
            .and_then(|c| c.take_custom_message_writer());
        // Keep the reader in this caller so it is restored even if retryable
        // upstream errors make try_join! cancel the downstream future.
        let mut downstream_custom_message_reader = match session
            .take_downstream_custom_message_reader(&mut downstream_custom_message_writer)
        {
            Ok(reader) => reader,
            Err(e) => return (false, false, Some(e)),
        };

        let mut upstream_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut downstream_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (tx_upstream, rx_upstream) = upstream_pipe.split();
        let (tx_downstream, rx_downstream) = downstream_pipe.split();

        session.as_mut().enable_retry_buffering();

        // Shared signal so the upstream half can distinguish an expected task-pipe
        // closure (the downstream half finished and dropped rx) from an unexpected one.
        let pipe_state = Arc::new(AtomicU8::new(PipeState::Active as u8));

        // start bi-directional streaming
        let ret = tokio::try_join!(
            self.proxy_handle_downstream(
                session,
                tx_downstream,
                rx_upstream,
                ctx,
                &mut downstream_custom_message_writer,
                &mut downstream_custom_message_reader,
                pipe_state.clone(),
            ),
            self.proxy_handle_upstream(client_session, tx_upstream, rx_downstream, pipe_state),
        );

        if let Some(custom_session) = session.downstream_session.as_custom_mut() {
            if let Some(downstream_custom_message_writer) = downstream_custom_message_writer {
                match custom_session.restore_custom_message_writer(downstream_custom_message_writer)
                {
                    Ok(_) => { /* continue */ }
                    Err(e) => {
                        return (false, false, Some(e));
                    }
                }
            }
            if let Some(downstream_custom_message_reader) = downstream_custom_message_reader {
                match custom_session.restore_custom_message_reader(downstream_custom_message_reader)
                {
                    Ok(_) => { /* continue */ }
                    Err(e) => {
                        return (false, false, Some(e));
                    }
                }
            }
        }

        match ret {
            Ok((downstream_can_reuse, upstream_can_reuse)) => {
                (downstream_can_reuse, upstream_can_reuse, None)
            }
            Err(e) => (false, false, Some(e)),
        }
    }

    pub(crate) async fn proxy_to_h1_upstream(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        reused: bool,
        peer: &HttpPeer,
        ctx: &mut SV::CTX,
    ) -> (bool, bool, Option<Box<Error>>)
    // (reuse_server, reuse_client, error)
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        #[cfg(windows)]
        let raw = client_session.id() as std::os::windows::io::RawSocket;
        #[cfg(unix)]
        let raw = client_session.id();

        let initial_write_pending = client_session.stream().get_write_pending_time();

        if let Err(e) = self
            .inner
            .connected_to_upstream(
                session,
                reused,
                peer,
                raw,
                Some(client_session.digest()),
                ctx,
            )
            .await
        {
            return (false, false, Some(e));
        }

        let (server_session_reuse, client_session_reuse, error) =
            self.proxy_1to1(session, client_session, peer, ctx).await;

        // Record upstream response body bytes received (payload only) for logging consumers.
        let upstream_bytes_total = client_session.body_bytes_received();
        session.set_upstream_body_bytes_received(upstream_bytes_total);

        // Record request body bytes written to the upstream (payload only) for logging consumers.
        // Only HTTP/1.x tracks this; see `Session::upstream_body_bytes_sent`.
        session.set_upstream_body_bytes_sent(client_session.body_bytes_sent());

        // Record upstream write pending time for this session only (delta from baseline).
        let current_write_pending = client_session.stream().get_write_pending_time();
        let upstream_write_pending = current_write_pending.saturating_sub(initial_write_pending);
        session.set_upstream_write_pending_time(upstream_write_pending);

        (server_session_reuse, client_session_reuse, error)
    }

    async fn proxy_handle_upstream(
        &self,
        client_session: &mut HttpSessionV1,
        tx: spsc::Sender<'_, HttpTask, TASK_BUFFER_SIZE>,
        mut rx: spsc::Receiver<'_, HttpTask, TASK_BUFFER_SIZE>,
        pipe_state: Arc<AtomicU8>,
    ) -> Result<bool>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut request_done = false;
        let mut response_done = false;
        let mut upstream_can_reuse = true;
        let mut send_error = None;
        let mut upgraded = false;

        /* duplex mode, wait for either to complete */
        while !request_done || !response_done {
            tokio::select! {
                res = client_session.read_response_task(), if !response_done => {
                    match res {
                        Ok(task) => {
                            response_done = task.is_end();
                            if !upgraded && client_session.was_upgraded() {
                                // upgrade can only happen once
                                upgraded = true;
                                if send_error.is_none() {
                                    // continue receiving from downstream after body mode change
                                    request_done = false;
                                }
                            }
                            let type_str = task.type_str();
                            let result = tx.send(task)
                                .await.or_err_with(
                                    InternalError,
                                    || format!("Failed to send upstream task {type_str}{} to pipe",
                                        if response_done { " (end)" } else {""})
                                );
                            // If the request is upgraded, the downstream pipe can early exit
                            // when the downstream connection is closed.
                            // In that case, this function should ignore that the pipe is closed.
                            // So that this function could read the rest events from rx including
                            // the closure, then exit.
                            if result.is_err() && !client_session.was_upgraded() {
                                if PipeState::is_downstream_complete(
                                    pipe_state.load(Ordering::Acquire),
                                ) {
                                    // The downstream half finished the response by its own framing
                                    // and dropped the task pipe, so this send failure is benign.
                                    if response_done {
                                        // The whole upstream response was already read off the
                                        // socket. Keep looping so the request side can finish; the
                                        // natural loop exit then decides reuse from both directions
                                        // (the connection is reusable only if the request was also
                                        // fully sent, which a premature response can leave undone).
                                        continue;
                                    }
                                    // We stopped mid-response, so the upstream connection has
                                    // unread bytes and is unsafe to reuse.
                                    return Ok(false);
                                }
                                // The pipe closed but the downstream half did not signal completion:
                                // this is an unexpected closure, so surface the original send error.
                                return Err(result.expect_err("send failure already checked via is_err() above"));
                            }
                        },
                        Err(e) => {
                            // Push the error to downstream and then quit
                            // Don't care if send fails: downstream already gone
                            let _ = tx.send(HttpTask::Failed(send_error.unwrap_or(e).into_up())).await;
                            // A response read error means the HTTP/1 message boundary was not
                            // established successfully, so the connection cannot be reused.
                            return Ok(false)
                        }
                    }
                },

                body = rx.recv(), if !request_done => {
                    match send_body_to1(client_session, body).await {
                        Ok(send_done) => {
                            request_done = send_done;
                            // An upgraded request is terminated when either side is done
                            if request_done && client_session.was_upgraded() {
                                response_done = true;
                            }
                        },
                        Err(e) => {
                           warn!("send error, draining read buf: {e}");
                           request_done = true;

                           // Built-in HTTP downstream sessions are expected to reject
                           // incomplete bodies before reaching this state. A downstream
                           // session that does not report such an error or an upstream request
                           // mutation that creates inconsistent framing can still reach it.
                           // A complete response can be forwarded, but the partially written
                           // HTTP/1 request makes this connection unsafe to reuse.
                           upstream_can_reuse = false;
                           send_error = Some(e);
                           continue
                        }
                    }
                },

                else => {
                    // this shouldn't be reached as the while loop would already exit
                    break;
                }
            }
        }

        Ok(upstream_can_reuse)
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_upstream_tasks(
        &self,
        session: &mut Session,
        ctx: &mut SV::CTX,
        initial_task: HttpTask,
        rx: &mut spsc::Receiver<'_, HttpTask, TASK_BUFFER_SIZE>,
        serve_from_cache: &mut ServeFromCache,
        range_body_filter: &mut proxy_cache::range_filter::RangeBodyFilter,
        response_state: &mut ResponseStateMachine,
    ) -> Result<Option<bool>>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        if serve_from_cache.should_discard_upstream() {
            // Serving the cached response and discarding the upstream one; nothing
            // is written downstream this round, so return None and let the caller
            // continue.
            return Ok(None);
        }

        // Batch: pull as many tasks as we can from rx
        let tasks = collect_available_tasks(initial_task, rx);

        /* run filters before sending to downstream */
        let mut filtered_tasks = HttpTaskBatch::new();
        for mut t in tasks {
            if self.revalidate_or_stale(session, &mut t, ctx).await {
                serve_from_cache.enable();
                response_state.enable_cached_response();
                // skip downstream filtering entirely as the 304 will not be sent
                break;
            }
            #[cfg(feature = "upstream_modules")]
            if let HttpTask::Header(header, end_of_stream) = &t {
                self.inner
                    .adjust_upstream_modules(session, header, *end_of_stream, ctx)
                    .await?;
            }
            #[cfg(feature = "upstream_modules")]
            session.upstream_modules_filter_task(&mut t).await?;
            session.upstream_compression.response_filter(&mut t);
            let task = self
                .h1_response_filter(session, t, ctx, serve_from_cache, range_body_filter, false)
                .await?;
            if serve_from_cache.is_miss_header() {
                response_state.enable_cached_response();
            }
            // check error and abort
            // otherwise the error is surfaced via write_response_tasks()
            if !serve_from_cache.should_send_to_downstream() {
                if let HttpTask::Failed(e) = task {
                    return Err(e);
                }
            }
            filtered_tasks.push(task);
        }

        if !serve_from_cache.should_send_to_downstream() {
            // TODO: need to derive response_done from filtered_tasks in case downstream failed already
            return Ok(None);
        }

        let response_done = session.write_response_task_batch(filtered_tasks).await?;

        Ok(Some(response_done))
    }

    // todo use this function to replace bidirection_1to2()
    // returns whether this server (downstream) session can be reused
    #[allow(clippy::too_many_arguments)]
    async fn proxy_handle_downstream(
        &self,
        session: &mut Session,
        tx: spsc::Sender<'_, HttpTask, TASK_BUFFER_SIZE>,
        mut rx: spsc::Receiver<'_, HttpTask, TASK_BUFFER_SIZE>,
        ctx: &mut SV::CTX,
        downstream_custom_message_writer: &mut Option<Box<dyn CustomMessageWrite>>,
        downstream_custom_message_reader: &mut Option<
            Box<dyn futures::Stream<Item = Result<Bytes>> + Unpin + Send + Sync + 'static>,
        >,
        pipe_state: Arc<AtomicU8>,
    ) -> Result<bool>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        // setup custom message forwarding, if downstream supports it
        let (
            mut downstream_custom_read,
            mut downstream_custom_write,
            downstream_custom_message_custom_forwarding,
            mut downstream_custom_message_inject_rx,
        ) = if downstream_custom_message_writer.is_some() {
            let (inject_tx, inject_rx) = mpsc::channel::<Bytes>(CUSTOM_MESSAGE_QUEUE_SIZE);
            (true, true, Some(inject_tx), Some(inject_rx))
        } else {
            (false, false, None, None)
        };

        if let Some(custom_forwarding) = downstream_custom_message_custom_forwarding {
            // Custom handles are owned by the caller so an early error here still
            // lets the caller restore them before retrying another upstream.
            self.inner
                .custom_forwarding(session, ctx, None, custom_forwarding)
                .await?;
        }

        let mut downstream_state = DownstreamStateMachine::new(session.as_mut().is_body_done());

        let buffer = session.as_ref().get_retry_buffer();

        // retry, send buffer if it exists or body empty
        if buffer.is_some() || session.as_mut().is_body_empty() {
            let send_permit = tx
                .reserve()
                .await
                .or_err(InternalError, "reserving body pipe")?;
            self.send_body_to_pipe(
                session,
                buffer,
                downstream_state.is_done(),
                send_permit,
                ctx,
            )
            .await?;
        }

        let mut response_state = ResponseStateMachine::new();

        // these two below can be wrapped into an internal ctx
        // use cache when upstream revalidates (or TODO: error)
        let mut serve_from_cache = proxy_cache::ServeFromCache::new();
        let mut range_body_filter = proxy_cache::range_filter::RangeBodyFilter::new();

        let mut next_upstream_task: Option<HttpTask> = None;

        /* duplex mode without caching
         * Read body from downstream while reading response from upstream
         * If response is done, only read body from downstream
         * If request is done, read response from upstream while idling downstream (to close quickly)
         * If both are done, quit the loop
         *
         * With caching + but without partial read support
         * Similar to above, cache admission write happen when the data is write to downstream
         *
         * With caching + partial read support
         * A. Read upstream response and write to cache
         * B. Read data from cache and send to downstream
         * If B fails (usually downstream close), continue A.
         * If A fails, exit with error.
         * If both are done, quit the loop
         * Usually there is no request body to read for cacheable request
         */
        while !downstream_state.is_done()
            || !response_state.is_done()
            || downstream_custom_read && !downstream_state.is_errored()
            || downstream_custom_write
        {
            // reserve tx capacity ahead to avoid deadlock, see below

            let send_permit = tx
                .try_reserve()
                .or_err(InternalError, "try_reserve() body pipe for upstream");

            // Use optional futures to allow using optional channels in select branches
            let custom_inject_rx_recv: OptionFuture<_> = downstream_custom_message_inject_rx
                .as_mut()
                .map(|rx| rx.recv())
                .into();
            let custom_reader_next: OptionFuture<_> = downstream_custom_message_reader
                .as_mut()
                .map(|reader| reader.next())
                .into();

            // partial read support, this check will also be false if cache is disabled.
            let support_cache_partial_read =
                session.cache.support_streaming_partial_write() == Some(true);
            let upgraded = session.was_upgraded();

            tokio::select! {
                // only try to send to pipe if there is capacity to avoid deadlock
                // Otherwise deadlock could happen if both upstream and downstream are blocked
                // on sending to their corresponding pipes which are both full.
                body = session.downstream_session.read_body_or_idle(downstream_state.is_done()),
                    if downstream_state.can_poll() && send_permit.is_ok() => {

                    debug!("downstream event");
                    let body = match body {
                        Ok(b) => b,
                        Err(e) => {
                            let wait_for_cache_fill = (!serve_from_cache.is_on() && support_cache_partial_read)
                                || serve_from_cache.is_miss();
                            if wait_for_cache_fill {
                                // ignore downstream error so that upstream can continue to write cache
                                downstream_state.to_errored();
                                if !self.inner.suppress_proxy_warn_log(
                                    session,
                                    ctx,
                                    &e,
                                    ProxyWarnLogContext::DownstreamCache,
                                ) {
                                    warn!(
                                        "Downstream Error ignored during caching: {}, {}",
                                        e,
                                        self.inner.request_summary(session, ctx)
                                    );
                                }
                                // This will not be treated as a final error, but we should signal to
                                // downstream session regardless
                                session.downstream_session.on_proxy_failure(e);
                                continue;
                           } else {
                                return Err(e.into_down());
                           }
                        }
                    };
                    // If the request is websocket, `None` body means the request is closed.
                    // Set the response to be done as well so that the request completes normally.
                    if body.is_none() && session.was_upgraded() {
                        response_state.maybe_set_upstream_done(true);
                    }
                    // TODO: consider just drain this if serve_from_cache is set
                    let is_body_done = session.is_body_done();
                    let request_done = self.send_body_to_pipe(
                        session,
                        body,
                        is_body_done,
                        send_permit.unwrap(), // safe because we checked is_ok()
                        ctx,
                    )
                    .await?;
                    downstream_state.maybe_finished(request_done);
                },

                _ = tx.reserve(), if downstream_state.is_reading() && send_permit.is_err() => {
                    // If tx is closed, the upstream has already finished its job.
                    downstream_state.maybe_finished(tx.is_closed());
                    debug!("waiting for permit {send_permit:?}, upstream closed {}", tx.is_closed());
                     /* No permit, wait on more capacity to avoid starving.
                     * Otherwise this select only blocks on rx, which might send no data
                     * before the entire body is uploaded.
                     * once more capacity arrives we just loop back
                     */
                },

                // Handle buffered upstream task from previous iteration
                task = async { next_upstream_task.take() }, if next_upstream_task.is_some() => {
                    debug!("buffered upstream event: {:?}", task);
                    if let Some(t) = task {
                        let Some(response_done) = self.process_upstream_tasks(
                            session,
                            ctx,
                            t,
                            &mut rx,
                            &mut serve_from_cache,
                            &mut range_body_filter,
                            &mut response_state,
                        ).await? else {
                            // nothing sent downstream e.g. serve_from_cache
                            continue;
                        };
                        response_state.maybe_set_upstream_done(response_done);
                        // unsuccessful upgrade response may force the request done
                        downstream_state.maybe_finished(session.is_body_done());
                    } else {
                        debug!("empty upstream event");
                        response_state.maybe_set_upstream_done(true);
                    }
                },

                task = rx.recv(), if !response_state.upstream_done() && next_upstream_task.is_none() => {
                    debug!("upstream event: {:?}", task);
                    if let Some(t) = task {
                        let upgraded = session.was_upgraded();
                        let Some(response_done) = self.process_upstream_tasks(
                            session,
                            ctx,
                            t,
                            &mut rx,
                            &mut serve_from_cache,
                            &mut range_body_filter,
                            &mut response_state,
                        ).await? else {
                            // nothing sent downstream e.g. serve_from_cache
                            continue;
                        };
                        if !upgraded && session.was_upgraded() && downstream_state.can_poll() {
                            // TODO: write can happen async now
                            // just upgraded, the downstream state should be reset to continue to
                            // poll body
                            trace!("reset downstream state on upgrade");
                            downstream_state.reset();
                        }
                        response_state.maybe_set_upstream_done(response_done);
                        // unsuccessful upgrade response (or end of upstream upgraded conn,
                        // which forces the body reader to complete) may force the request done
                        downstream_state.maybe_finished(session.is_body_done());
                    } else {
                        debug!("empty upstream event");
                        response_state.maybe_set_upstream_done(true);
                    }
                },

                task = serve_from_cache.next_http_task(&mut session.cache, &mut range_body_filter, upgraded),
                    if !response_state.cached_done()
                        && !downstream_state.is_errored()
                        && serve_from_cache.is_on()
                        && !session.has_pending_downstream_tasks() => { // backpressure: don't queue if pending writes

                    let task = self.h1_response_filter(session, task?, ctx,
                        &mut serve_from_cache,
                        &mut range_body_filter, true).await?;
                    debug!("serve_from_cache task {task:?}");

                    if session.downstream_session.supports_proxy_task_api() {
                        session.send_downstream_proxy_task(task).await?;
                    } else {
                        match session.write_response_task_batch(cache_task_batch(task)).await {
                            Ok(b) => response_state.maybe_set_cache_done(b),
                            Err(e) => if serve_from_cache.is_miss() {
                                // give up writing to downstream but wait for upstream cache write to finish
                                downstream_state.to_errored();
                                response_state.maybe_set_cache_done(true);
                                if !self.inner.suppress_proxy_warn_log(
                                    session,
                                    ctx,
                                    &e,
                                    ProxyWarnLogContext::DownstreamCache,
                                ) {
                                    warn!(
                                        "Downstream Error ignored during caching: {}, {}",
                                        e,
                                        self.inner.request_summary(session, ctx)
                                    );
                                }
                                // This will not be treated as a final error, but we should signal to
                                // downstream session regardless
                                session.downstream_session.on_proxy_failure(e);
                                continue;
                            } else {
                                return Err(e);
                            }
                        }
                        // A storage error can disable cache between cached_done
                        // being set and here; disable() drops the enabled_ctx so
                        // finish_hit_handler would panic without this guard.
                        if response_state.cached_done() && session.cache.enabled() {
                            if let Err(e) = session.cache.finish_hit_handler().await {
                                warn!("Error during finish_hit_handler: {}", e);
                            }
                        }
                    }
                }

                // Write queued downstream proxy tasks while also polling for upstream tasks.
                // This allows cache writes to continue even when downstream is stalled.
                //
                // "Gate" branch: ready(()) resolves immediately, so the guard controls
                // whether we enter. This is not a busy-loop because every path through
                // the inner select either (a) drains all pending tasks via
                // write_downstream_proxy_tasks (making the guard false), (b) observes a
                // downstream write error (making downstream_state errored and the guard false),
                // (c) stores an upstream task in next_upstream_task (making the guard false), or
                // (d) blocks on real I/O inside the nested select.
                _ = std::future::ready(()),
                    if !downstream_state.is_errored()
                        && session.has_pending_downstream_tasks()
                        && next_upstream_task.is_none() => {
                    tokio::select! {
                        // Try to write downstream proxy tasks (cancel-safe)
                        write_result = session.write_downstream_proxy_tasks() => {
                            match write_result {
                                Ok(end) => {
                                    response_state.maybe_set_cache_done(end);
                                    // See enabled() guard comment above.
                                    if response_state.cached_done() && session.cache.enabled() {
                                        if let Err(e) = session.cache.finish_hit_handler().await {
                                            warn!("Error during finish_hit_handler: {}", e);
                                        }
                                    }
                                }
                                Err(e) => if serve_from_cache.is_miss() {
                                    // give up writing to downstream but wait for upstream cache write to finish
                                    downstream_state.to_errored();
                                    response_state.maybe_set_cache_done(true);
                                    if !self.inner.suppress_proxy_warn_log(
                                        session,
                                        ctx,
                                        &e,
                                        ProxyWarnLogContext::DownstreamCache,
                                    ) {
                                        warn!(
                                            "Downstream write error ignored during caching: {}, {}",
                                            e,
                                            self.inner.request_summary(session, ctx)
                                        );
                                    }
                                    // This will not be treated as a final error, but we should signal to
                                    // downstream session regardless
                                    session.downstream_session.on_proxy_failure(e);
                                } else {
                                    return Err(e);
                                }
                            }
                        }

                        // Also poll for upstream tasks - if we get one, cancel the write and handle it.
                        // Only poll if there is no buffered task already waiting to be processed.
                        upstream_task = rx.recv(), if !response_state.upstream_done() && serve_from_cache.is_on() && next_upstream_task.is_none() => {
                            if let Some(t) = upstream_task {
                                // Store this upstream task to be processed next iteration
                                next_upstream_task = Some(t);
                                continue;
                            } else {
                                response_state.maybe_set_upstream_done(true);
                            }
                        }
                    }
                }

                data = custom_reader_next, if downstream_custom_read && !downstream_state.is_errored()  => {
                    let Some(data) = data.flatten() else {
                        downstream_custom_read = false;
                        continue;
                    };

                    let data = match data {
                        Ok(data) => data,
                        Err(err) =>  {
                            warn!("downstream_custom_message_reader got error: {err}");
                            downstream_custom_read = false;
                            continue;
                        },
                    };

                    self.inner
                        .downstream_custom_message_proxy_filter(session, data, ctx, true) // true, because it's the last hop for downstream proxying
                        .await?;
                },

                data = custom_inject_rx_recv, if downstream_custom_write => {
                    match data.flatten() {
                        Some(data) => {
                            if let Some(ref mut custom_writer) = downstream_custom_message_writer {
                                custom_writer.write_custom_message(data).await?
                            }
                        },
                        None => {
                            downstream_custom_write = false;
                            if let Some(ref mut custom_writer) = downstream_custom_message_writer {
                                custom_writer.finish_custom().await?;
                            }
                        },
                    }
                },

                else => {
                    break;
                }
            }
        }

        let mut reuse_downstream = !downstream_state.is_errored();
        if reuse_downstream {
            match session.as_mut().finish_body().await {
                Ok(_) => {
                    debug!("finished sending body to downstream");
                }
                Err(e) => {
                    error!("Error finish sending body to downstream: {}", e);
                    reuse_downstream = false;
                }
            }
        }
        // Signal the upstream half that the downstream half completed cleanly before
        // dropping rx, so a resulting task-pipe closure is treated as benign.
        pipe_state.store(PipeState::DownstreamComplete as u8, Ordering::Release);
        Ok(reuse_downstream)
    }

    pub(crate) async fn h1_response_filter(
        &self,
        session: &mut Session,
        mut task: HttpTask,
        ctx: &mut SV::CTX,
        serve_from_cache: &mut ServeFromCache,
        range_body_filter: &mut RangeBodyFilter,
        from_cache: bool, // are the task from cache already
    ) -> Result<HttpTask>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        // skip caching if already served from cache
        if !from_cache {
            if let Some(duration) = self.upstream_filter(session, &mut task, ctx).await? {
                trace!("delaying upstream response for {duration:?}");
                time::sleep(duration).await;
            }

            if let HttpTask::Header(header, _) = &task {
                reject_mismatched_h1_upgrade_101(session, header, "h1_upstream_filter")
                    .map_err(|e| e.into_up())?;
            }

            // cache the original response before any downstream transformation
            // requests that bypassed cache still need to run filters to see if the response has become cacheable
            if session.cache.enabled() || session.cache.bypassing() {
                if let Err(e) = self
                    .cache_http_task(session, &task, ctx, serve_from_cache)
                    .await
                {
                    session.cache.disable(NoCacheReason::StorageError);
                    if serve_from_cache.is_miss_body() {
                        // if the response stream cache body during miss but write fails, it has to
                        // give up the entire request
                        return Err(e);
                    } else {
                        // otherwise, continue processing the response
                        warn!(
                            "Fail to cache response: {}, {}",
                            e,
                            self.inner.request_summary(session, ctx)
                        );
                    }
                }
            }

            if !serve_from_cache.should_send_to_downstream() {
                return Ok(task);
            }
        } // else: cached/local response, no need to trigger upstream filters and caching

        // normally max file size is tracked in cache_http_task filters (when cache enabled),
        // we will track it in these filters before sending to downstream on specific conditions
        // when cache is disabled
        let track_max_cache_size = matches!(
            session.cache.phase(),
            CachePhase::Disabled(NoCacheReason::PredictedResponseTooLarge)
        );

        let res = match task {
            HttpTask::Header(mut header, end) => {
                /* Downstream revalidation/range, only needed when cache modified headers because otherwise origin
                 * will handle it */
                if session.upstream_headers_mutated_for_cache() {
                    self.downstream_response_conditional_filter(
                        serve_from_cache,
                        session,
                        &mut header,
                        ctx,
                    );
                    if !session.ignore_downstream_range {
                        let range_type = self.inner.range_header_filter(session, &mut header, ctx);
                        range_body_filter.set(range_type);
                    }
                }

                // TODO: just set version to Version::HTTP_11 unconditionally here,
                // (with another todo being an option to faithfully proxy the <1.1 responses)
                // as we are already trying to mutate this for HTTP/1.1 downstream reuse

                /* Convert HTTP 1.0 style response to chunked encoding so that we don't
                 * have to close the downstream connection */
                // these status codes / method cannot have body, so no need to add chunked encoding
                let no_body = session.req_header().method == http::method::Method::HEAD
                    || matches!(header.status.as_u16(), 204 | 304);
                if !no_body
                    && !header.status.is_informational()
                    && header
                        .headers
                        .get(http::header::TRANSFER_ENCODING)
                        .is_none()
                    && header.headers.get(http::header::CONTENT_LENGTH).is_none()
                    && !end
                {
                    // Upgrade the http version to 1.1 because 1.0/0.9 doesn't support chunked
                    header.set_version(Version::HTTP_11);
                    header.insert_header(http::header::TRANSFER_ENCODING, "chunked")?;
                }

                self.inner
                    .response_filter(session, &mut header, ctx)
                    .await?;
                if !from_cache {
                    // Re-check after response_filter in case it changed the final status to 101.
                    reject_mismatched_h1_upgrade_101(session, &header, "h1_response_filter")
                        .map_err(|e| e.into_in())?;
                }
                Ok(HttpTask::Header(header, end))
            }
            HttpTask::Body(data, end) => {
                if track_max_cache_size {
                    session
                        .cache
                        .track_body_bytes_for_max_file_size(data.as_ref().map_or(0, |d| d.len()));
                }

                // before it can mark it as cacheable again.
                let mut data = range_body_filter.filter_body(data);
                if let Some(duration) = self
                    .inner
                    .response_body_filter(session, &mut data, end, ctx)?
                {
                    trace!("delaying downstream response for {:?}", duration);
                    time::sleep(duration).await;
                }

                Ok(HttpTask::Body(data, end))
            }
            HttpTask::UpgradedBody(mut data, end) => {
                if track_max_cache_size {
                    session
                        .cache
                        .track_body_bytes_for_max_file_size(data.as_ref().map_or(0, |d| d.len()));
                }

                // range doesn't apply to upgraded body
                if let Some(duration) = self
                    .inner
                    .response_body_filter(session, &mut data, end, ctx)?
                {
                    trace!("delaying downstream upgraded response for {:?}", duration);
                    time::sleep(duration).await;
                }

                Ok(HttpTask::UpgradedBody(data, end))
            }
            HttpTask::Trailer(h) => Ok(HttpTask::Trailer(h)), // TODO: support trailers for h1
            HttpTask::Done => Ok(task),
            HttpTask::Failed(_) => Ok(task), // Do nothing just pass the error down
        };
        // On end, check if the response (based on file size) can be considered cacheable again
        if let Ok(task) = res.as_ref() {
            if track_max_cache_size
                && task.is_end()
                && !matches!(task, HttpTask::Failed(_))
                && !session.cache.exceeded_max_file_size()
            {
                session.cache.response_became_cacheable();
            }
        }
        res
    }

    // TODO:: use this function to replace send_body_to2
    async fn send_body_to_pipe(
        &self,
        session: &mut Session,
        mut data: Option<Bytes>,
        end_of_body: bool,
        tx: spsc::Permit<'_, HttpTask, TASK_BUFFER_SIZE>,
        ctx: &mut SV::CTX,
    ) -> Result<bool>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        // None: end of body
        // this var is to signal if downstream finish sending the body, which shouldn't be
        // affected by the request_body_filter
        let end_of_body = end_of_body || data.is_none();

        session
            .downstream_modules_ctx
            .request_body_filter(&mut data, end_of_body)
            .await?;

        // TODO: request body filter to have info about upgraded status?
        // (can also check session.was_upgraded())
        self.inner
            .request_body_filter(session, &mut data, end_of_body, ctx)
            .await?;

        // the flag to signal to upstream
        let upstream_end_of_body = end_of_body || data.is_none();

        /* It is normal to get 0 bytes because of multi-chunk or request_body_filter decides not to
         * output anything yet.
         * Don't write 0 bytes to the network since it will be
         * treated as the terminating chunk */
        if !upstream_end_of_body && data.as_ref().is_some_and(|d| d.is_empty()) {
            return Ok(false);
        }

        debug!(
            "Read {} bytes body from downstream",
            data.as_ref().map_or(-1, |d| d.len() as isize)
        );

        // upgraded body needs to be marked
        if session.was_upgraded() {
            tx.send(HttpTask::UpgradedBody(data, upstream_end_of_body));
        } else {
            tx.send(HttpTask::Body(data, upstream_end_of_body));
        }

        Ok(end_of_body)
    }
}

pub(crate) async fn send_body_to1(
    client_session: &mut HttpSessionV1,
    recv_task: Option<HttpTask>,
) -> Result<bool> {
    let body_done;

    if let Some(task) = recv_task {
        match task {
            HttpTask::Body(data, end) => {
                body_done = end;
                if let Some(d) = data {
                    let m = client_session.write_body(&d).await;
                    match m {
                        Ok(m) => match m {
                            Some(n) => {
                                debug!("Write {} bytes body to upstream", n);
                            }
                            None => {
                                warn!("Upstream body is already finished. Nothing to write");
                            }
                        },
                        Err(e) => {
                            return e.into_up().into_err();
                        }
                    }
                }
            }
            HttpTask::UpgradedBody(data, end) => {
                client_session.maybe_upgrade_body_writer();

                body_done = end;
                if let Some(d) = data {
                    let m = client_session.write_body(&d).await;
                    match m {
                        Ok(m) => {
                            match m {
                                Some(n) => {
                                    debug!("Write {} bytes upgraded body to upstream", n);
                                }
                                None => {
                                    warn!("Upstream upgraded body is already finished. Nothing to write");
                                }
                            }
                        }
                        Err(e) => {
                            return e.into_up().into_err();
                        }
                    }
                }
            }
            _ => {
                // should never happen, sender only sends body
                warn!("Unexpected task sent to upstream");
                body_done = true;
                // error here,
                // for client sessions that received upgrade but didn't
                // receive any UpgradedBody,
                // no more data is arriving so we should consider this
                // as downstream finalizing its upgrade payload
                client_session.maybe_upgrade_body_writer();
            }
        }
    } else {
        // sender dropped
        body_done = true;
        // for client sessions that received upgrade but didn't
        // receive any UpgradedBody,
        // no more data is arriving so we should consider this
        // as downstream finalizing its upgrade payload
        client_session.maybe_upgrade_body_writer();
    }

    if body_done {
        match client_session.finish_body().await {
            Ok(_) => {
                debug!("finish sending body to upstream");
                Ok(true)
            }
            Err(e) => e.into_up().into_err(),
        }
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod task_buffer_tests {
    use super::*;

    fn body_task(value: u8) -> HttpTask {
        HttpTask::Body(Some(Bytes::from(vec![value])), false)
    }

    fn body_value(task: &HttpTask) -> Option<u8> {
        let HttpTask::Body(Some(body), _) = task else {
            return None;
        };
        body.first().copied()
    }

    async fn collect_batch(values: &[u8]) -> HttpTaskBatch {
        let mut channel = spsc::Channel::new();
        let (tx, mut rx) = channel.split();
        for value in values {
            tx.send(body_task(*value)).await.unwrap();
        }
        let first = rx.recv().await.unwrap();
        collect_available_tasks(first, &mut rx)
    }

    #[tokio::test]
    async fn batches_through_channel_capacity_stay_inline() {
        for len in 1..=TASK_BUFFER_SIZE {
            let values: Vec<_> = (0..len as u8).collect();
            let tasks = collect_batch(&values).await;

            assert_eq!(tasks.len(), len);
            assert!(!tasks.spilled(), "batch of {len} tasks spilled");
        }
    }

    #[tokio::test]
    async fn available_batch_can_spill_without_losing_order() {
        let mut channel = spsc::Channel::new();
        let (tx, mut rx) = channel.split();
        for value in 0..TASK_BUFFER_SIZE as u8 {
            tx.send(body_task(value)).await.unwrap();
        }
        let first = rx.recv().await.unwrap();
        tx.try_reserve()
            .unwrap()
            .send(body_task(TASK_BUFFER_SIZE as u8));

        let tasks = collect_available_tasks(first, &mut rx);

        assert!(tasks.spilled());
        assert_eq!(
            tasks.iter().filter_map(body_value).collect::<Vec<_>>(),
            (0..=TASK_BUFFER_SIZE as u8).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn batch_snapshot_preserves_filter_order_and_failure() {
        let mut channel = spsc::Channel::new();
        let (tx, mut rx) = channel.split();
        tx.send(body_task(1)).await.unwrap();
        tx.send(body_task(2)).await.unwrap();
        tx.send(body_task(3)).await.unwrap();
        tx.send(HttpTask::Failed(Error::new(InternalError)))
            .await
            .unwrap();

        let first = rx.recv().await.unwrap();
        let tasks = collect_available_tasks(first, &mut rx);
        tx.send(body_task(4)).await.unwrap();

        let mut filtered = Vec::new();
        let mut failure = None;
        for task in tasks {
            match task {
                HttpTask::Body(Some(body), _) if body[0] % 2 == 1 => filtered.push(body[0]),
                HttpTask::Failed(error) => {
                    failure = Some(error);
                    break;
                }
                _ => {}
            }
        }

        assert_eq!(filtered, vec![1, 3]);
        assert_eq!(failure.unwrap().etype(), &InternalError);
        assert_eq!(body_value(&rx.recv().await.unwrap()), Some(4));
    }

    #[test]
    fn cache_singleton_batch_preserves_task_variants_inline() {
        let body = cache_task_batch(HttpTask::Body(Some(Bytes::from_static(b"body")), true));
        assert!(!body.spilled());
        assert!(matches!(
            &body[0],
            HttpTask::Body(Some(data), true) if data.as_ref() == b"body"
        ));

        let mut error = Error::new(InternalError);
        error.as_up();
        error.set_retry(true);
        let failed = cache_task_batch(HttpTask::Failed(error));
        assert!(!failed.spilled());
        let HttpTask::Failed(error) = &failed[0] else {
            panic!("failure task changed type");
        };
        assert_eq!(error.etype(), &InternalError);
        assert_eq!(error.esource(), &ErrorSource::Upstream);
        assert!(error.retry());

        let mut headers = http::HeaderMap::new();
        headers.insert("x-trailer", http::HeaderValue::from_static("yes"));
        let trailer = cache_task_batch(HttpTask::Trailer(Some(Box::new(headers))));
        assert!(!trailer.spilled());
        assert!(matches!(
            &trailer[0],
            HttpTask::Trailer(Some(headers))
                if headers.get("x-trailer").is_some_and(|value| value == "yes")
        ));

        let upgraded = cache_task_batch(HttpTask::UpgradedBody(
            Some(Bytes::from_static(b"upgraded")),
            false,
        ));
        assert!(!upgraded.spilled());
        assert!(matches!(
            &upgraded[0],
            HttpTask::UpgradedBody(Some(data), false) if data.as_ref() == b"upgraded"
        ));
    }
}

#[cfg(test)]
mod pump_saturation_tests {
    use std::{sync::Arc, time::Duration};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;
    use tokio_test::io::Builder;

    use super::*;

    struct PumpProxy;

    impl ProxyHttp for PumpProxy {
        type CTX = ();

        fn new_ctx(&self) -> Self::CTX {}

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unreachable!("pump tests do not select an upstream")
        }
    }

    fn proxy() -> HttpProxy<PumpProxy> {
        HttpProxy::new(PumpProxy, Arc::new(ServerConf::default()))
    }

    fn pipe_state() -> Arc<AtomicU8> {
        Arc::new(AtomicU8::new(PipeState::Active as u8))
    }

    fn response_mock(chunks: &[&'static [u8]]) -> tokio_test::io::Mock {
        let mut builder = Builder::new();
        builder.read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        for chunk in chunks {
            builder.read(chunk);
        }
        builder.build()
    }

    fn task_marker(task: HttpTask) -> u8 {
        match task {
            HttpTask::Header(_, _) => b'H',
            HttpTask::Body(Some(body), _) => body[0],
            HttpTask::Body(None, true) => b'E',
            HttpTask::Failed(_) => b'F',
            task => panic!("unexpected pump task: {task:?}"),
        }
    }

    #[tokio::test]
    async fn response_pump_blocks_at_four_tasks_and_resumes_in_fifo_order() {
        let mock = response_mock(&[
            b"1\r\na\r\n",
            b"1\r\nb\r\n",
            b"1\r\nc\r\n",
            b"1\r\nd\r\n",
            b"1\r\ne\r\n",
            b"1\r\nf\r\n",
            b"0\r\n\r\n",
        ]);
        let mut upstream = HttpSessionV1::new(Box::new(mock));
        let proxy = proxy();
        let mut response_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut request_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (response_tx, mut response_rx) = response_pipe.split();
        let (request_tx, request_rx) = request_pipe.split();
        drop(request_tx);
        let handler =
            proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx, pipe_state());
        tokio::pin!(handler);

        assert!(
            timeout(Duration::from_millis(20), handler.as_mut())
                .await
                .is_err(),
            "response pump did not block on full handoff"
        );
        let mut markers = Vec::new();
        for _ in 0..TASK_BUFFER_SIZE {
            markers.push(task_marker(response_rx.try_recv().unwrap()));
        }
        assert_eq!(markers, b"Habc");
        assert!(matches!(
            response_rx.try_recv(),
            Err(spsc::TryRecvError::Empty)
        ));

        timeout(Duration::from_millis(100), handler.as_mut())
            .await
            .expect("response pump did not resume")
            .unwrap();
        while let Ok(task) = response_rx.try_recv() {
            markers.push(task_marker(task));
        }
        assert_eq!(markers, b"HabcdefE");
        assert!(matches!(
            response_rx.try_recv(),
            Err(spsc::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn response_failure_waits_behind_queued_tasks_without_reordering() {
        let mock = response_mock(&[
            b"1\r\na\r\n",
            b"1\r\nb\r\n",
            b"1\r\nc\r\n",
            b"not-a-chunk\r\n",
        ]);
        let mut upstream = HttpSessionV1::new(Box::new(mock));
        let proxy = proxy();
        let mut response_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut request_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (response_tx, mut response_rx) = response_pipe.split();
        let (request_tx, request_rx) = request_pipe.split();
        drop(request_tx);
        let handler =
            proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx, pipe_state());
        tokio::pin!(handler);

        assert!(
            timeout(Duration::from_millis(20), handler.as_mut())
                .await
                .is_err(),
            "failed task did not wait behind the full handoff"
        );
        let mut markers = Vec::new();
        for _ in 0..TASK_BUFFER_SIZE {
            markers.push(task_marker(response_rx.try_recv().unwrap()));
        }
        assert_eq!(markers, b"Habc");

        timeout(Duration::from_millis(100), handler.as_mut())
            .await
            .expect("failed response pump did not resume")
            .unwrap();
        while let Ok(task) = response_rx.try_recv() {
            markers.push(task_marker(task));
        }
        assert_eq!(markers, b"HabcF");
    }

    #[tokio::test]
    async fn downstream_disconnect_wakes_response_pump_blocked_on_full_handoff() {
        let mock = response_mock(&[b"1\r\na\r\n", b"1\r\nb\r\n", b"1\r\nc\r\n", b"1\r\nd\r\n"]);
        let mut upstream = HttpSessionV1::new(Box::new(mock));
        let proxy = proxy();
        let mut response_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut request_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (response_tx, response_rx) = response_pipe.split();
        let (request_tx, request_rx) = request_pipe.split();
        drop(request_tx);
        let handler =
            proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx, pipe_state());
        tokio::pin!(handler);

        assert!(
            timeout(Duration::from_millis(20), handler.as_mut())
                .await
                .is_err(),
            "response pump did not block on full handoff"
        );
        drop(response_rx);
        assert!(timeout(Duration::from_millis(100), handler.as_mut())
            .await
            .expect("blocked response sender missed receiver drop")
            .is_err());
    }

    #[tokio::test]
    async fn saturated_request_handoff_handles_early_final_and_upstream_reset() {
        let (proxy_io, mut origin_io) = tokio::io::duplex(256);
        let origin = tokio::spawn(async move {
            let mut request_head = Vec::new();
            while !request_head.ends_with(b"\r\n\r\n") {
                request_head.push(origin_io.read_u8().await.unwrap());
            }
            origin_io
                .write_all(
                    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let mut upstream = HttpSessionV1::new(Box::new(proxy_io));
        let mut request = RequestHeader::build("POST", b"/upload", None).unwrap();
        request
            .insert_header(header::CONTENT_LENGTH, "1024")
            .unwrap();
        upstream.write_request_header_ref(&request).await.unwrap();

        let proxy = proxy();
        let mut response_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut request_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (response_tx, mut response_rx) = response_pipe.split();
        let (request_tx, request_rx) = request_pipe.split();
        for value in 0..TASK_BUFFER_SIZE {
            let value = u8::try_from(value).expect("task buffer index fits in u8");
            request_tx
                .try_reserve()
                .unwrap()
                .send(HttpTask::Body(Some(Bytes::from(vec![value; 128])), false));
        }
        assert_eq!(
            request_tx.try_reserve().unwrap_err(),
            spsc::TryReserveError::Full
        );

        timeout(
            Duration::from_millis(100),
            proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx, pipe_state()),
        )
        .await
        .expect("early-final/reset pump timed out")
        .unwrap();
        origin.await.unwrap();
        let HttpTask::Header(response, true) = response_rx.try_recv().unwrap() else {
            panic!("early final response was not preserved");
        };
        assert_eq!(response.status, http::StatusCode::PAYLOAD_TOO_LARGE);
        assert!(request_tx.is_closed());
    }
}

#[cfg(test)]
mod request_preparation_tests {
    use std::{
        any::Any,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::SystemTime,
    };

    use pingora_cache::{
        meta::CacheMeta,
        storage::{HitHandler, MissHandler, PurgeOutcome, PurgeTarget, PurgeType, Storage},
        trace::SpanHandle,
        CacheKey,
    };
    use tokio_test::io::Builder;

    use super::*;
    use pingora_core::protocols::http::v1::client::HttpSession as ClientSession;

    struct CloneProbe(Arc<AtomicUsize>);

    impl Clone for CloneProbe {
        fn clone(&self) -> Self {
            self.0.fetch_add(1, Ordering::Relaxed);
            Self(Arc::clone(&self.0))
        }
    }

    struct DefaultProxy;

    impl ProxyHttp for DefaultProxy {
        type CTX = ();

        fn new_ctx(&self) -> Self::CTX {}

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unreachable!("request preparation does not select a peer")
        }
    }

    struct TestStorage;

    #[async_trait]
    impl Storage for TestStorage {
        async fn lookup(
            &'static self,
            _key: &CacheKey,
            _trace: &SpanHandle,
        ) -> Result<Option<(CacheMeta, HitHandler)>> {
            unreachable!("request preparation does not access cache storage")
        }

        async fn get_miss_handler(
            &'static self,
            _key: &CacheKey,
            _meta: &CacheMeta,
            _trace: &SpanHandle,
        ) -> Result<MissHandler> {
            unreachable!("request preparation does not access cache storage")
        }

        async fn purge(
            &'static self,
            _target: PurgeTarget<'_>,
            _purge_type: PurgeType,
            _trace: &SpanHandle,
        ) -> Result<PurgeOutcome> {
            unreachable!("request preparation does not access cache storage")
        }

        async fn update_meta(
            &'static self,
            _key: &CacheKey,
            _meta: &CacheMeta,
            _trace: &SpanHandle,
        ) -> Result<bool> {
            unreachable!("request preparation does not access cache storage")
        }

        fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
            self
        }
    }

    static TEST_STORAGE: TestStorage = TestStorage;

    struct BorrowingProxy;

    impl ProxyHttp for BorrowingProxy {
        type CTX = ();

        fn new_ctx(&self) -> Self::CTX {}

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unreachable!("request preparation does not select a peer")
        }

        async fn prepare_upstream_request(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<PreparedUpstreamRequest> {
            Ok(PreparedUpstreamRequest::Borrowed)
        }
    }

    fn preserve() -> HttpUpstreamRequestPolicy {
        HttpUpstreamRequestPolicy::preserve()
    }

    async fn session_for(request: &'static [u8]) -> Session {
        let mock = Builder::new().read(request).build();
        let mut session = Session::new_h1(Box::new(mock));
        session.read_request().await.unwrap();
        session
    }

    async fn session_with_probe() -> (Session, Arc<AtomicUsize>) {
        let mut session = session_for(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions_mut()
            .insert(CloneProbe(Arc::clone(&clones)));
        (session, clones)
    }

    #[tokio::test]
    async fn default_preparation_clones_and_owns_plain_h1() {
        let (mut session, clones) = session_with_probe().await;

        let prepared =
            prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut (), preserve())
                .await
                .unwrap();

        assert!(matches!(prepared, PreparedUpstreamRequest::Owned(_)));
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn preserve_policy_borrows_an_unchanged_request_without_cloning() {
        let (mut session, clones) = session_with_probe().await;

        let prepared =
            prepare_h1_upstream_request(&BorrowingProxy, &mut session, &mut (), preserve())
                .await
                .unwrap();

        assert!(matches!(prepared, PreparedUpstreamRequest::Borrowed));
        assert_eq!(clones.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn standard_policy_strips_hop_by_hop_fields_from_a_borrowed_request() {
        let mut session = session_for(
            b"GET / HTTP/1.1\r\nHost: example.test\r\nConnection: keep-alive, X-Nominated\r\n\
              X-Nominated: 1\r\nKeep-Alive: timeout=5\r\nTE: trailers\r\nProxy-Authorization: Basic abc\r\n\
              X-Kept: yes\r\n\r\n",
        )
        .await;

        let prepared = prepare_h1_upstream_request(
            &BorrowingProxy,
            &mut session,
            &mut (),
            HttpUpstreamRequestPolicy::standard(),
        )
        .await
        .unwrap();

        let PreparedUpstreamRequest::Owned(request) = prepared else {
            panic!("a policy that edits the request must own it");
        };
        for name in [
            "connection",
            "x-nominated",
            "keep-alive",
            "te",
            "proxy-authorization",
        ] {
            assert!(!request.headers.contains_key(name), "{name} was forwarded");
        }
        assert_eq!(request.headers["host"], "example.test");
        assert_eq!(request.headers["x-kept"], "yes");
        assert!(session.req_header().headers.contains_key("connection"));
    }

    #[tokio::test]
    async fn standard_policy_keeps_a_chunked_request_framed() {
        let mut session = session_for(
            b"POST / HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        )
        .await;

        let prepared = prepare_h1_upstream_request(
            &BorrowingProxy,
            &mut session,
            &mut (),
            HttpUpstreamRequestPolicy::standard(),
        )
        .await
        .unwrap();

        let PreparedUpstreamRequest::Owned(request) = prepared else {
            panic!("a policy that edits the request must own it");
        };
        assert_eq!(request.headers["transfer-encoding"], "chunked");
    }

    #[tokio::test]
    async fn h2_conversion_clones_before_default_filter_preparation() {
        let (mut session, clones) = session_with_probe().await;
        session.req_header_mut().set_version(Version::HTTP_2);

        let prepared =
            prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut (), preserve())
                .await
                .unwrap();

        let PreparedUpstreamRequest::Owned(request) = prepared else {
            panic!("HTTP/2 conversion must own the request");
        };
        assert_eq!(request.version, Version::HTTP_11);
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn active_cache_mutation_clones_before_default_filter_preparation() {
        let (mut session, clones) = session_with_probe().await;
        assert!(!session.req_header().has_case());
        session.cache.enable(&TEST_STORAGE, None, None, None, None);
        session.cache.set_cache_key(CacheKey::new("request", ""));
        session.cache.cache_miss();

        let prepared =
            prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut (), preserve())
                .await
                .unwrap();

        let PreparedUpstreamRequest::Owned(request) = prepared else {
            panic!("active cache mutation must own the request");
        };
        assert!(!request.has_case());
        assert!(session.upstream_headers_mutated_for_cache());
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn parsed_h1_response_enters_cache_without_case_map() {
        let response = b"HTTP/1.1 200 OK\r\nX-CaChEd: retained\r\nContent-Length: 0\r\n\r\n";
        let mock = Builder::new().read(response).build();
        let mut client = ClientSession::new(Box::new(mock));
        client.read_response().await.unwrap();
        let response = client.resp_header().unwrap().clone();
        assert!(!response.has_case());

        let now = SystemTime::now();
        let meta = CacheMeta::new(now, now, 0, 0, response);
        assert!(!meta.response_header().has_case());
        assert_eq!(meta.headers()["x-cached"], "retained");
        let cached = meta.response_header_copy();
        assert!(!cached.has_case());
        let mut wire = Vec::new();
        cached.header_to_h1_wire(&mut wire);
        assert_eq!(wire, b"x-cached: retained\r\nContent-Length: 0\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;

    struct ResponseFilter101;

    impl ProxyHttp for ResponseFilter101 {
        type CTX = ();

        fn new_ctx(&self) -> Self::CTX {}

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unreachable!("test calls h1_response_filter directly")
        }

        async fn response_filter(
            &self,
            _session: &mut Session,
            response: &mut ResponseHeader,
            _ctx: &mut Self::CTX,
        ) -> Result<()> {
            response.set_status(http::StatusCode::SWITCHING_PROTOCOLS)?;
            response.set_version(Version::HTTP_11);
            Ok(())
        }
    }

    async fn upgrade_request_session() -> Session {
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(
                b"GET / HTTP/1.1\r\nHost: example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .await
            .expect("test request should be written");

        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        session
            .read_request()
            .await
            .expect("test request should parse");
        session
    }

    #[tokio::test]
    async fn h1_response_filter_rejects_filter_created_101_with_upgrade_mismatch() {
        let proxy = HttpProxy::new(ResponseFilter101, Arc::new(ServerConf::default()));
        let mut session = upgrade_request_session().await;
        session.h1_upgrade_request_status = H1UpgradeRequestStatus {
            upstream: Some(false),
        };

        let mut ctx = ();
        let mut serve_from_cache = ServeFromCache::new();
        let mut range_body_filter = RangeBodyFilter::new();
        let task = HttpTask::Header(Box::new(ResponseHeader::build(200, Some(0)).unwrap()), true);

        let err = proxy
            .h1_response_filter(
                &mut session,
                task,
                &mut ctx,
                &mut serve_from_cache,
                &mut range_body_filter,
                false,
            )
            .await
            .unwrap_err();

        assert_eq!(err.etype(), &InvalidHTTPHeader);
        assert_eq!(err.esource(), &ErrorSource::Internal);
    }

    #[tokio::test]
    async fn h1_response_filter_rejects_upstream_101_upgrade_mismatch_as_upstream() {
        let proxy = HttpProxy::new(ResponseFilter101, Arc::new(ServerConf::default()));
        let mut session = upgrade_request_session().await;
        session.h1_upgrade_request_status = H1UpgradeRequestStatus {
            upstream: Some(false),
        };

        let mut ctx = ();
        let mut serve_from_cache = ServeFromCache::new();
        let mut range_body_filter = RangeBodyFilter::new();
        let mut response =
            ResponseHeader::build(http::StatusCode::SWITCHING_PROTOCOLS, Some(0)).unwrap();
        response.set_version(Version::HTTP_11);
        let task = HttpTask::Header(Box::new(response), true);

        let err = proxy
            .h1_response_filter(
                &mut session,
                task,
                &mut ctx,
                &mut serve_from_cache,
                &mut range_body_filter,
                false,
            )
            .await
            .unwrap_err();

        assert_eq!(err.etype(), &InvalidHTTPHeader);
        assert_eq!(err.esource(), &ErrorSource::Upstream);
    }
}
