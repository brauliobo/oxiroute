//! Cancels the upstream attempt of an in-flight request so it can be dispatched to another server.
//!
//! Every registered request owns one `Evacuation`: a small compare-and-swap state machine the proxy and
//! the operator race on. The proxy commits to a response by `Movable -> Pinned(ResponseStarted)`; the
//! operator takes the request by `Movable -> Moving` (or `Pinned -> Killing` when forcing). Exactly one
//! wins, so a request is either moved before anything reached the client or finishes untouched.

use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use pingora::{Error, ErrorType, proxy::UpstreamCancellation};
use tokio::sync::Notify;

mod report;
#[cfg(test)]
mod tests;

pub use report::{EvacuationReport, LeftRunning};

/// How many times one request can be moved before it is left to finish where it is.
pub(crate) const MAX_EVACUATIONS: usize = 3;
pub(crate) const EVACUATED: ErrorType = ErrorType::Custom("evacuated");

pub(crate) fn is_evacuated(error: &Error) -> bool {
    error.etype() == &EVACUATED
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    /// No attempt in progress: queued, in retry wait or finished.
    Idle,
    Movable,
    Pinned(LeftRunning),
    Moving,
    Killing,
}

impl State {
    const PINNED: u8 = 4;

    const fn encode(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Movable => 1,
            Self::Moving => 2,
            Self::Killing => 3,
            Self::Pinned(reason) => Self::PINNED + reason as u8,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Movable,
            2 => Self::Moving,
            3 => Self::Killing,
            _ => Self::Pinned(LeftRunning::ALL[usize::from(value - Self::PINNED)]),
        }
    }

    const fn cancelled(self) -> bool {
        matches!(self, Self::Moving | Self::Killing)
    }
}

/// What the operator did to one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fate {
    Redispatched,
    Forced,
    LeftRunning(LeftRunning),
}

/// What the proxy must do with the attempt an operator cancelled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Cancellation {
    Move,
    Kill,
}

pub(crate) struct Evacuation {
    state: AtomicU8,
    redispatched: AtomicUsize,
    notify: Notify,
}

impl Evacuation {
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicU8::new(State::Idle.encode()),
            redispatched: AtomicUsize::new(0),
            notify: Notify::new(),
        }
    }

    fn load(&self) -> State {
        State::decode(self.state.load(Ordering::Acquire))
    }

    fn transition(&self, from: State, to: State) -> Result<(), State> {
        self.state
            .compare_exchange(
                from.encode(),
                to.encode(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(drop)
            .map_err(State::decode)
    }

    /// How many times the request has been moved to another server.
    pub(crate) fn redispatches(&self) -> usize {
        self.redispatched.load(Ordering::Acquire)
    }

    /// An attempt starts: it can move unless its request or the move limit forbids it.
    pub(crate) fn begin_attempt(&self, unmovable: Option<LeftRunning>) {
        let limit = (self.redispatches() >= MAX_EVACUATIONS).then_some(LeftRunning::LimitReached);
        let state = unmovable.or(limit).map_or(State::Movable, State::Pinned);
        self.state.store(state.encode(), Ordering::Release);
    }

    /// The upstream response header is about to reach the client. False when the operator took the
    /// request first, in which case nothing may be sent.
    pub(crate) fn commit_response(&self) -> bool {
        match self.transition(State::Movable, State::Pinned(LeftRunning::ResponseStarted)) {
            Ok(()) => true,
            Err(state) => !state.cancelled(),
        }
    }

    /// The request cannot be replayed any more, for example because its body outgrew the retry buffer.
    pub(crate) fn pin(&self, reason: LeftRunning) {
        let _ = self.transition(State::Movable, State::Pinned(reason));
    }

    /// The attempt ended. Reports what the operator decided, if anything, and leaves the request idle.
    pub(crate) fn settle(&self) -> Option<Cancellation> {
        match State::decode(self.state.swap(State::Idle.encode(), Ordering::AcqRel)) {
            State::Moving => {
                self.redispatched.fetch_add(1, Ordering::AcqRel);
                Some(Cancellation::Move)
            }
            State::Killing => Some(Cancellation::Kill),
            State::Idle | State::Movable | State::Pinned(_) => None,
        }
    }

    /// The operator's side. `None` when the request has no attempt to act on.
    pub(crate) fn evacuate(&self, force: bool, has_destination: bool) -> Option<Fate> {
        loop {
            let state = self.load();
            let (next, fate) = match state {
                State::Idle | State::Moving | State::Killing => return None,
                State::Movable if has_destination => (State::Moving, Fate::Redispatched),
                State::Movable => return Some(Fate::LeftRunning(LeftRunning::NoDestination)),
                State::Pinned(_) if force => (State::Killing, Fate::Forced),
                State::Pinned(reason) => return Some(Fate::LeftRunning(reason)),
            };
            if self.transition(state, next).is_ok() {
                self.notify.notify_one();
                return Some(fate);
            }
        }
    }

    /// Resolves once the operator cancelled the attempt. A permit left over from an earlier
    /// cancellation only wakes the loop, which checks the state again.
    pub(crate) fn cancellation(self: &Arc<Self>) -> UpstreamCancellation {
        let handle = Arc::clone(self);
        Box::pin(async move {
            while !handle.load().cancelled() {
                handle.notify.notified().await;
            }
            Error::new_in(EVACUATED)
        })
    }
}
