#[allow(clippy::wildcard_imports)]
use super::*;
use crate::http::evacuation::Evacuation;

impl HttpRequestContext {
    pub(super) fn evacuation(&self) -> Option<&Evacuation> {
        self.inflight.as_ref().map(InflightGuard::evacuation)
    }

    /// Lets the operator move or kill the attempt about to run, until it commits to a response.
    pub(super) fn arm_evacuation(&self, session: &Session) -> Option<UpstreamCancellation> {
        let inflight = self.inflight.as_ref()?;
        inflight
            .evacuation()
            .begin_attempt(self.unmovable(request_body_replayable(session)));
        Some(Box::pin(inflight.cancelled()))
    }

    /// Lets the operator move or kill the attempt of the epoll data plane about to run; the engine thread that
    /// serves the request is woken when the operator acts.
    #[cfg(target_os = "linux")]
    pub(super) fn arm_evacuation_for_engine(&self, body_replayable: bool) {
        if let Some(evacuation) = self.evacuation() {
            evacuation.begin_attempt(self.unmovable(body_replayable));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn bind_evacuation_to_engine(&self, waker: &oxiroute_epoll_h1::Waker) {
        if let Some(evacuation) = self.evacuation() {
            evacuation.bind_engine(waker);
        }
    }

    fn unmovable(&self, body_replayable: bool) -> Option<LeftRunning> {
        self.evacuation_pin
            .or_else(|| (!body_replayable).then_some(LeftRunning::BodyNotReplayable))
    }

    /// The upstream response is about to reach the client. Fails when the operator took the request
    /// first, so nothing is sent. Informational responses other than `101` do not commit.
    pub(super) fn commit_response(&self, status: StatusCode) -> pingora::Result<()> {
        let informational = status.is_informational() && status != StatusCode::SWITCHING_PROTOCOLS;
        let taken = !informational
            && self
                .evacuation()
                .is_some_and(|evacuation| !evacuation.commit_response());
        if taken {
            Err(Error::new_in(EVACUATED))
        } else {
            Ok(())
        }
    }

    pub(super) fn pin_evacuation(&self, reason: LeftRunning) {
        if let Some(evacuation) = self.evacuation() {
            evacuation.pin(reason);
        }
    }

    /// The attempt ended. What the operator decided about it, if anything, takes precedence over the
    /// error that ended it.
    pub(super) fn cancelled_by_operator(&mut self) -> Option<Box<Error>> {
        let cancellation = self.evacuation()?.settle()?;
        self.release_lease();
        let mut error = Error::new_in(EVACUATED);
        match cancellation {
            Cancellation::Move => self.move_attempt(),
            Cancellation::Kill => self.trace.pending_outcome = Some(AttemptOutcome::Evacuated),
        }
        error.set_retry(cancellation == Cancellation::Move);
        Some(error)
    }

    /// The server stays in `attempted_upstreams`, so every exclusion keeps working, but the move is
    /// neither a retry nor a failure: no budget, no attempt, no delay, no passive health signal.
    fn move_attempt(&mut self) {
        self.evacuated_from.extend(self.trace.stats.clone());
        self.trace.finish(AttemptOutcome::Evacuated, None);
        self.evacuations += 1;
    }

    /// A moved request that ends without a response failed to be re-dispatched.
    pub(super) fn record_failed_evacuations(&self, error: Option<&Error>) {
        if error.is_some_and(|error| error.esource() != &ErrorSource::Downstream) {
            for stats in &self.evacuated_from {
                stats.record_evacuation(EvacuationResult::Failed, 1);
            }
        }
    }
}
