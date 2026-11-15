use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Moment(u64);

fn nanos(span: Duration) -> u64 {
    u64::try_from(span.as_nanos()).unwrap_or(u64::MAX)
}

impl Moment {
    pub(crate) const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub(crate) const fn nanos(self) -> u64 {
        self.0
    }

    pub(crate) fn after(self, span: Duration) -> Self {
        Self(self.0.saturating_add(nanos(span)))
    }

    pub(crate) fn since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    base: Instant,
}

impl Clock {
    pub(crate) fn start() -> Self {
        Self {
            base: Instant::now(),
        }
    }

    pub(crate) fn moment(self, at: Instant) -> Moment {
        Moment(nanos(at.saturating_duration_since(self.base)))
    }
}
