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
use pingora_core::protocols::http::custom::CUSTOM_MESSAGE_QUEUE_SIZE;

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

async fn prepare_h1_upstream_request<SV>(
    inner: &SV,
    session: &mut Session,
    ctx: &mut SV::CTX,
) -> Result<PreparedUpstreamRequest>
where
    SV: ProxyHttp + Send + Sync,
    SV::CTX: Send + Sync,
{
    let requires_owned = session.req_header().version == Version::HTTP_2 || session.cache.enabled();
    if !requires_owned {
        return inner.prepare_upstream_request(session, ctx).await;
    }

    let mut req = session.req_header().clone();

    // Convert HTTP2 headers to H1
    if req.version == Version::HTTP_2 {
        req.set_version(Version::HTTP_11);
        // if client has body but has no content length, add chunked encoding
        // https://datatracker.ietf.org/doc/html/rfc9112#name-message-body
        // "The presence of a message body in a request is signaled by a Content-Length or Transfer-Encoding header field."
        if !session.is_body_empty() && session.get_header(header::CONTENT_LENGTH).is_none() {
            req.insert_header(header::TRANSFER_ENCODING, "chunked")
                .unwrap();
        }
        if session.get_header(header::HOST).is_none() {
            // H2 is required to set :authority, but no necessarily header
            // most H1 server expect host header, so convert
            let host = req.uri.authority().map_or("", |a| a.as_str()).to_owned();
            req.insert_header(header::HOST, host).unwrap();
        }
        // TODO: Add keepalive header for connection reuse, but this is not required per RFC
    }

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
    Ok(PreparedUpstreamRequest::Owned(Box::new(req)))
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
        client_session.write_timeout = peer.options.write_timeout;

        // phase 2 send to upstream

        let prepared = match prepare_h1_upstream_request(&self.inner, session, ctx).await {
            Ok(prepared) => prepared,
            Err(e) => return (false, true, Some(e)),
        };

        let req = match &prepared {
            PreparedUpstreamRequest::Borrowed => session.downstream_session.req_header(),
            PreparedUpstreamRequest::Owned(req) => req,
        };
        session.upstream_compression.request_filter(req);

        debug!("Sending header to upstream {:?}", req);

        match client_session.write_request_header_ref(req).await {
            Ok(_) => { /* Continue */ }
            Err(e) => {
                return (false, false, Some(e.into_up()));
            }
        }

        let mut downstream_custom_message_writer = session
            .downstream_session
            .as_custom_mut()
            .and_then(|c| c.take_custom_message_writer());

        let mut upstream_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let mut downstream_pipe = spsc::Channel::<HttpTask, TASK_BUFFER_SIZE>::new();
        let (tx_upstream, rx_upstream) = upstream_pipe.split();
        let (tx_downstream, rx_downstream) = downstream_pipe.split();

        session.as_mut().enable_retry_buffering();

        // start bi-directional streaming
        let ret = tokio::try_join!(
            self.proxy_handle_downstream(
                session,
                tx_downstream,
                rx_upstream,
                ctx,
                &mut downstream_custom_message_writer
            ),
            self.proxy_handle_upstream(client_session, tx_upstream, rx_downstream),
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
        }

        match ret {
            Ok((downstream_can_reuse, _upstream)) => (downstream_can_reuse, true, None),
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
    ) -> Result<()>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        let mut request_done = false;
        let mut response_done = false;
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
                                return result;
                            }
                        },
                        Err(e) => {
                            // Push the error to downstream and then quit
                            // Don't care if send fails: downstream already gone
                            let _ = tx.send(HttpTask::Failed(send_error.unwrap_or(e).into_up())).await;
                            // Downstream should consume all remaining data and handle the error
                            return Ok(())
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

        Ok(())
    }

    // todo use this function to replace bidirection_1to2()
    // returns whether this server (downstream) session can be reused
    async fn proxy_handle_downstream(
        &self,
        session: &mut Session,
        tx: spsc::Sender<'_, HttpTask, TASK_BUFFER_SIZE>,
        mut rx: spsc::Receiver<'_, HttpTask, TASK_BUFFER_SIZE>,
        ctx: &mut SV::CTX,
        downstream_custom_message_writer: &mut Option<Box<dyn CustomMessageWrite>>,
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
            mut downstream_custom_message_reader,
        ) = if downstream_custom_message_writer.is_some() {
            let reader = session.downstream_custom_message()?;
            let (inject_tx, inject_rx) = mpsc::channel::<Bytes>(CUSTOM_MESSAGE_QUEUE_SIZE);
            (true, true, Some(inject_tx), Some(inject_rx), reader)
        } else {
            (false, false, None, None, None)
        };

        if let Some(custom_forwarding) = downstream_custom_message_custom_forwarding {
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
                                warn!(
                                    "Downstream Error ignored during caching: {}, {}",
                                    e,
                                    self.inner.request_summary(session, ctx)
                                );
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

                task = rx.recv(), if !response_state.upstream_done() => {
                    debug!("upstream event: {:?}", task);
                    if let Some(t) = task {
                        if serve_from_cache.should_discard_upstream() {
                            // just drain, do we need to do anything else?
                           continue;
                        }
                        // pull as many tasks as we can
                        let tasks = collect_available_tasks(t, &mut rx);

                        /* run filters before sending to downstream */
                        let mut filtered_tasks = HttpTaskBatch::new();
                        for mut t in tasks {
                            if self.revalidate_or_stale(session, &mut t, ctx).await {
                                serve_from_cache.enable();
                                response_state.enable_cached_response();
                                // skip downstream filtering entirely as the 304 will not be sent
                                break;
                            }
                            session.upstream_compression.response_filter(&mut t);
                            let task = self.h1_response_filter(session, t, ctx,
                                &mut serve_from_cache,
                                &mut range_body_filter, false).await?;
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
                            continue;
                        }

                        // set to downstream
                        let upgraded = session.was_upgraded();
                        let response_done = session.write_response_task_batch(filtered_tasks).await?;
                        if !upgraded && session.was_upgraded() && downstream_state.can_poll() {
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
                    if !response_state.cached_done() && !downstream_state.is_errored() && serve_from_cache.is_on() => {

                    let task = self.h1_response_filter(session, task?, ctx,
                        &mut serve_from_cache,
                        &mut range_body_filter, true).await?;
                    debug!("serve_from_cache task {task:?}");

                    match session.write_response_task_batch(cache_task_batch(task)).await {
                        Ok(b) => response_state.maybe_set_cache_done(b),
                        Err(e) => if serve_from_cache.is_miss() {
                            // give up writing to downstream but wait for upstream cache write to finish
                            downstream_state.to_errored();
                            response_state.maybe_set_cache_done(true);
                            warn!(
                                "Downstream Error ignored during caching: {}, {}",
                                e,
                                self.inner.request_summary(session, ctx)
                            );
                            // This will not be treated as a final error, but we should signal to
                            // downstream session regardless
                            session.downstream_session.on_proxy_failure(e);
                            continue;
                        } else {
                            return Err(e);
                        }
                    }
                    if response_state.cached_done() {
                        if let Err(e) = session.cache.finish_hit_handler().await {
                            warn!("Error during finish_hit_handler: {}", e);
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

        if let Some(custom_session) = session.downstream_session.as_custom_mut() {
            if let Some(downstream_custom_message_reader) = downstream_custom_message_reader {
                custom_session
                    .restore_custom_message_reader(downstream_custom_message_reader)
                    .expect("downstream restore_custom_message_reader should be empty");
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
        Ok(reuse_downstream)
    }

    async fn h1_response_filter(
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

                match self.inner.response_filter(session, &mut header, ctx).await {
                    Ok(_) => Ok(HttpTask::Header(header, end)),
                    Err(e) => Err(e),
                }
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

    #[async_trait]
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
        let handler = proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx);
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
        let handler = proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx);
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
        let handler = proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx);
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
            proxy.proxy_handle_upstream(&mut upstream, response_tx, request_rx),
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
        key::CompactCacheKey,
        meta::CacheMeta,
        storage::{HitHandler, MissHandler, PurgeType, Storage},
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

    #[async_trait]
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
            _key: &CompactCacheKey,
            _purge_type: PurgeType,
            _trace: &SpanHandle,
        ) -> Result<bool> {
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

    async fn session_with_probe() -> (Session, Arc<AtomicUsize>) {
        let request = b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n";
        let mock = Builder::new().read(request).build();
        let mut session = Session::new_h1(Box::new(mock));
        session.read_request().await.unwrap();
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions
            .insert(CloneProbe(Arc::clone(&clones)));
        (session, clones)
    }

    #[tokio::test]
    async fn default_preparation_clones_and_owns_plain_h1() {
        let (mut session, clones) = session_with_probe().await;

        let prepared = prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut ())
            .await
            .unwrap();

        assert!(matches!(prepared, PreparedUpstreamRequest::Owned(_)));
        assert_eq!(clones.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn h2_conversion_clones_before_default_filter_preparation() {
        let (mut session, clones) = session_with_probe().await;
        session.req_header_mut().set_version(Version::HTTP_2);

        let prepared = prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut ())
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
        session
            .cache
            .set_cache_key(CacheKey::new("test", "request", ""));
        session.cache.cache_miss();

        let prepared = prepare_h1_upstream_request(&DefaultProxy, &mut session, &mut ())
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
