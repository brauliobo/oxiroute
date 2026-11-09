//! The HTTP/1 relay of a request that has no body.
//!
//! [`HttpProxy::proxy_1to1`] runs the two directions of a request as two futures that talk through two channels,
//! because a request body and a response body can flow at the same time. A request without a body has only one
//! direction left: read the upstream response and write it to the client. That is done here in one loop that takes
//! the tasks straight from the upstream session, runs the same filters on them and writes them in the same batches,
//! without the channels and without a second future to poll.

use futures::FutureExt;

use super::*;
use crate::proxy_cache::{range_filter::RangeBodyFilter, ServeFromCache};

impl<SV, C> HttpProxy<SV, C>
where
    C: custom::Connector,
{
    /// Whether the exchange can use [`Self::proxy_1to1_bodyless`]: an HTTP/1 client without a request body,
    /// upgrade or cached response.
    pub(crate) fn relays_bodyless(session: &mut Session) -> bool {
        session.downstream_session.as_http1().is_some()
            && !session.cache.enabled()
            && !session.cache.bypassing()
            && !session.is_upgrade_req()
            && session.as_mut().is_body_empty()
    }

    /// Same result as [`Self::proxy_1to1`] for a request that passed [`Self::relays_bodyless`], once the request
    /// header is written upstream.
    pub(crate) async fn proxy_1to1_bodyless(
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
        match self.relay_bodyless(session, client_session, ctx).await {
            Ok(reuse_downstream) => (reuse_downstream, true, None),
            Err(error) => (false, false, Some(error)),
        }
    }

    async fn relay_bodyless(
        &self,
        session: &mut Session,
        client_session: &mut HttpSessionV1,
        ctx: &mut SV::CTX,
    ) -> Result<bool>
    where
        SV: ProxyHttp + Send + Sync,
        SV::CTX: Send + Sync,
    {
        // The end of the empty request body goes through the body filters like any other chunk, and ends the
        // upstream request.
        let mut body = None;
        session
            .downstream_modules_ctx
            .request_body_filter(&mut body, true)
            .await?;
        self.inner
            .request_body_filter(session, &mut body, true, ctx)
            .await?;
        let mut send_error = match client_session.finish_body().await {
            Ok(_) => None,
            Err(e) => {
                warn!("send error, draining read buf: {e}");
                Some(e.into_up())
            }
        };

        let mut serve_from_cache = ServeFromCache::new();
        let mut range_body_filter = RangeBodyFilter::new();
        loop {
            let first = tokio::select! {
                biased;
                task = client_session.read_response_task() => task,
                // The client is not expected to send anything before the response: closing the connection ends
                // the request.
                idle = session.downstream_session.read_body_or_idle(true) => {
                    match idle {
                        Err(e) => return Err(e.into_down()),
                        Ok(_) => continue,
                    }
                }
            };
            let mut tasks = HttpTaskBatch::new();
            let mut ended;
            match first {
                Ok(task) => {
                    ended = task.is_end();
                    tasks.push(task);
                }
                Err(e) => {
                    tasks.push(HttpTask::Failed(send_error.take().unwrap_or(e).into_up()));
                    ended = true;
                }
            }
            // Whatever the upstream session already holds goes out in the same write.
            while !ended && !tasks.spilled() && tasks.len() < TASK_BUFFER_SIZE {
                match client_session.read_response_task().now_or_never() {
                    Some(Ok(task)) => {
                        ended = task.is_end();
                        tasks.push(task);
                    }
                    Some(Err(e)) => {
                        tasks.push(HttpTask::Failed(send_error.take().unwrap_or(e).into_up()));
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
                        &mut serve_from_cache,
                        &mut range_body_filter,
                        false,
                    )
                    .await?;
                filtered_tasks.push(task);
            }
            let response_done = session.write_response_task_batch(filtered_tasks).await?;
            if response_done || ended {
                break;
            }
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
}
