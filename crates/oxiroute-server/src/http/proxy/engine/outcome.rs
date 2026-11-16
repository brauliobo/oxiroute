use http::StatusCode;
use oxiroute_epoll_h1::{Answered, Failure, Outcome, Recovery};
use pingora::{Error, ErrorType};

use super::exchange::Exchange;
use crate::{
    HealthFailure,
    http::proxy::{
        attempt::Exchanged,
        failure_reason,
        lane::{
            LaneFront,
            log::{HTTP1, Proxied},
            proxy::{Progress, failure_response},
            response::refused_by_pool,
        },
        proxy_policy,
        retry::status_retry_possible,
    },
    observability::upstream_stats::AttemptOutcome,
    upstream_peer::POOL_UNAVAILABLE,
};

pub(super) struct Pending {
    error: Box<Error>,
    status: Option<u16>,
}

impl Pending {
    fn status(status: u16, refused: bool) -> Self {
        let mut error = if refused {
            Error::new_up(POOL_UNAVAILABLE)
        } else {
            Error::new_up(ErrorType::HTTPStatus(status))
        };
        error.set_retry(true);
        Self {
            error,
            status: Some(status),
        }
    }

    const fn broken(error: Box<Error>) -> Self {
        Self {
            error,
            status: None,
        }
    }
}

#[allow(clippy::unnecessary_box_returns)]
fn error_of(failure: Failure) -> Box<Error> {
    match failure {
        Failure::NoServer => Error::new_up(POOL_UNAVAILABLE),
        Failure::Connect => Error::new_up(ErrorType::ConnectError),
        Failure::ConnectTimeout => Error::new_up(ErrorType::ConnectTimedout),
        Failure::NoCapacity => Error::new_up(ErrorType::HTTPStatus(503)),
        Failure::UpstreamWrite => Error::new_up(ErrorType::WriteError),
        Failure::UpstreamClosed => Error::explain(
            ErrorType::ConnectionClosed,
            "while reading response headers",
        )
        .into_up(),
        Failure::HeaderTimeout => {
            Error::explain(ErrorType::ReadTimedout, "while reading response headers").into_up()
        }
        Failure::BodyTimeout => Error::explain(ErrorType::ReadTimedout, "reading body").into_up(),
        Failure::TotalTimeout => Error::new_up(ErrorType::ReadTimedout),
        Failure::UnexpectedStatus => Error::new_up(ErrorType::HTTPStatus(502)),
        Failure::BadResponse | Failure::ResponseHeadTooLarge => {
            Error::new_up(ErrorType::InvalidHTTPHeader)
        }
        Failure::RequestHeadTooLarge => Error::new_down(ErrorType::InvalidHTTPHeader),
        Failure::Cancelled => Error::new_up(ErrorType::ConnectionClosed),
        Failure::ClientGone | Failure::Shutdown => Error::new_down(ErrorType::ConnectionClosed),
        Failure::ClientTimeout => Error::new_down(ErrorType::ReadTimedout),
    }
}

impl Exchange {
    #[allow(clippy::unnecessary_box_returns)]
    pub(super) fn retire(&mut self, pending: Pending) -> Box<Error> {
        if let Some(status) = pending.status {
            self.ctx.retry_prepaid = true;
            self.ctx
                .trace
                .finish(AttemptOutcome::StatusRetry, Some(status));
        }
        self.ctx
            .attempt_failed(pending.error, Exchanged::UNANSWERED)
    }

    pub(super) fn judge(&mut self, answered: &Answered<'_>) -> Recovery {
        match self.open(answered) {
            Ok(pending) => pending.map_or(Recovery::Answer, |pending| {
                self.pending = Some(pending);
                Recovery::Retry
            }),
            Err(error) => {
                self.pending = Some(Pending::broken(error));
                Recovery::Retry
            }
        }
    }

    fn open(&mut self, answered: &Answered<'_>) -> pingora::Result<Option<Pending>> {
        let ctx = &mut *self.ctx;
        let status = answered.head.status;
        ctx.now = answered.connected.unwrap_or(answered.now);
        ctx.connected(true, None)?;
        ctx.now = answered.now;
        ctx.ensure_open_at(answered.now, true)?;
        let refused = refused_by_pool(answered.bytes, answered.head);
        let retries_on_status = refused || proxy_policy(ctx).retries_on_status(status);
        if retries_on_status
            && answered.retryable
            && status_retry_possible(ctx, refused, false)
            && ctx.spend_retry_budget()
        {
            return Ok(Some(Pending::status(status, refused)));
        }
        ctx.commit_response(StatusCode::from_u16(status).expect("a scanned status"))?;
        Self::record_answer(ctx, status, retries_on_status, answered);
        Ok(None)
    }

    fn record_answer(
        ctx: &mut crate::http::proxy::HttpRequestContext,
        status: u16,
        unexpected: bool,
        answered: &Answered<'_>,
    ) {
        if unexpected {
            ctx.record_passive_failure(HealthFailure::UnexpectedStatus);
        } else {
            ctx.record_passive_success();
        }
        ctx.note_response(status, answered.now);
        ctx.enter_phase(crate::http::inflight::Phase::Streaming, answered.now);
    }

    pub(super) fn recover(&mut self, failure: Failure) -> Recovery {
        if self.settle(failure, Exchanged::UNANSWERED).retry() {
            Recovery::Retry
        } else {
            Recovery::Answer
        }
    }

    fn settle(&mut self, failure: Failure, exchanged: Exchanged) -> &Error {
        let ctx = &mut *self.ctx;
        self.error.get_or_insert_with(|| {
            let error = error_of(failure);
            match failure {
                Failure::Connect | Failure::ConnectTimeout | Failure::NoCapacity => {
                    ctx.connect_failed(error)
                }
                _ => ctx.attempt_failed(error, exchanged),
            }
        })
    }

    pub(super) fn answer(
        &mut self,
        failure: Failure,
        request_is_head: bool,
        out: &mut Vec<u8>,
    ) -> Option<u16> {
        self.settle(failure, Self::unreplayable(None));
        self.reasoned = true;
        let error = self.error.as_deref().expect("a failure was settled");
        let (code, bytes, body) =
            failure_response(&mut self.ctx, error, &self.id, request_is_head, None).ok()?;
        out.extend_from_slice(&bytes);
        self.answered_body = body;
        Some(code)
    }

    const fn unreplayable(status: Option<u16>) -> Exchanged {
        Exchanged {
            body_replayable: false,
            response_retryable: status.is_none(),
        }
    }

    pub(super) fn finish(mut self, front: &LaneFront, outcome: &Outcome) {
        if let Some(failure) = outcome.failure {
            self.settle(failure, Self::unreplayable(outcome.status));
        }
        if let Some(pending) = self.pending.take() {
            self.relayed(pending, outcome);
        }
        if let Some(error) = self.error.as_deref().filter(|_| !self.reasoned) {
            self.ctx.trace.failure_reason =
                Some(failure_reason(error, &self.ctx, outcome.status.is_some()));
        }
        let progress = Progress {
            received: usize::try_from(outcome.received).unwrap_or(usize::MAX),
            sent: usize::try_from(outcome.sent).unwrap_or(usize::MAX) + self.answered_body,
            status: outcome.status,
        };
        let request = Proxied {
            idempotency: self.idempotency.as_deref(),
            id: &self.id,
            id_generated: self.lane.client_id.is_none(),
            method: &self.method,
            route: &self.route,
            protocol: HTTP1,
        };
        front.account(&mut self.ctx, &request, self.error.as_deref(), &progress);
    }

    fn relayed(&mut self, pending: Pending, outcome: &Outcome) {
        match pending.status {
            Some(status) => {
                self.ctx
                    .record_passive_failure(HealthFailure::UnexpectedStatus);
                self.ctx.note_response(status, outcome.ended);
            }
            None => self.error = Some(pending.error),
        }
    }
}
