use std::time::{Duration, Instant};

/// Monotonic timestamp in a clock-specific epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonoTime(Duration);

impl MonoTime {
    #[must_use]
    pub const fn from_duration(value: Duration) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    #[must_use]
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// Injectable monotonic clock used for resident-age calculations and deterministic tests.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> MonoTime;
}

/// Process-local monotonic clock backed by [`Instant`].
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> MonoTime {
        MonoTime(self.origin.elapsed())
    }
}
