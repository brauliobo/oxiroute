//! What the proxy accounts for when a request ends, whatever protocol it arrived by: the operation, the attempt and the
//! access line.

use std::time::{Duration, Instant};

use log::warn;
use oxiroute_h1_head::RequestHead;
use pingora::Error;

use super::{
    log::{Proxied, write_proxied},
    proxy::Progress,
};
use crate::http::proxy::{HttpRequestContext, classify_http_result};

impl super::LaneFront {
    /// The idempotency value the access line of a request carries; a service without an access log reads none.
    pub(in crate::http::proxy) fn idempotency_of<'a>(
        &self,
        ctx: &HttpRequestContext,
        buffer: &'a [u8],
        head: &RequestHead,
    ) -> Option<&'a [u8]> {
        self.service.access_log()?;
        super::log::idempotency_of(ctx, buffer, head)
    }

    /// What the classic `logging` hook accounts for when a request ends at `ended`: the operation, the attempt and
    /// the access line.
    pub(in crate::http::proxy) fn account(
        &self,
        ctx: &mut HttpRequestContext,
        request: &Proxied<'_>,
        error: Option<&Error>,
        progress: &Progress,
        ended: Instant,
    ) {
        if let Some(error) = error
            && !ctx.left_unsent(error)
        {
            warn!(
                "HTTP request failed with {:?} from {:?}",
                error.etype(),
                error.esource()
            );
        }
        let elapsed = Self::finish_request(ctx, error, progress, ended);
        if let Some(log) = self.service.access_log() {
            write_proxied(log, &self.service_log, ctx, request, progress, elapsed);
        }
    }

    /// The accounting of a request that ended at `ended`, whatever protocol it arrived by; the time it took.
    pub(in crate::http::proxy) fn finish_request(
        ctx: &mut HttpRequestContext,
        error: Option<&Error>,
        progress: &Progress,
        ended: Instant,
    ) -> Duration {
        let (status, sent, received) = (progress.status, progress.sent, progress.received);
        ctx.set_clock(ended);
        let elapsed = ended.saturating_duration_since(ctx.started_at);
        let result = classify_http_result(error, status);
        ctx.operation_result = Some(result);
        if let Err(metric_error) = ctx.record_operation(result, elapsed) {
            warn!("could not account for HTTP operation metrics: {metric_error}");
        }
        ctx.observe_bytes(received, sent, ended);
        ctx.release_lease();
        ctx.account_attempt(received, sent, ended);
        ctx.record_failed_evacuations(error);
        elapsed
    }
}
