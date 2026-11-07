#[allow(clippy::wildcard_imports)]
use super::*;

const BUCKETS: usize = 10;
const CLOSED: u8 = 0;
const HALF_OPEN: u8 = 1;
const OPEN: u8 = 2;

/// Failure-ratio circuit breaker settings of a pool's servers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CircuitBreakerPolicy {
    window_ms: u64,
    min_requests: u32,
    failure_ratio_percent: u8,
    open_ms: u64,
    half_open_max: u16,
    half_open_successes: u16,
}

impl From<&CircuitBreaker> for CircuitBreakerPolicy {
    fn from(config: &CircuitBreaker) -> Self {
        Self {
            window_ms: config.window_ms,
            min_requests: config.min_requests,
            failure_ratio_percent: config.failure_ratio_percent,
            open_ms: config.open_ms,
            half_open_max: config.half_open_max,
            half_open_successes: config.half_open_successes,
        }
    }
}

/// What a recorded outcome changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BreakerTransition {
    Opened,
    Closed,
}

#[derive(Debug, Default)]
struct Window {
    bucket: u64,
    outcomes: [(u32, u32); BUCKETS],
}

impl Window {
    fn add(&mut self, ok: bool, now_unix_ms: u64, policy: CircuitBreakerPolicy) {
        let bucket = now_unix_ms / (policy.window_ms / BUCKETS as u64).max(1);
        let elapsed = bucket.saturating_sub(self.bucket);
        if elapsed >= BUCKETS as u64 {
            self.outcomes = [(0, 0); BUCKETS];
        } else {
            for skipped in 1..=elapsed {
                let slot = usize::try_from((self.bucket + skipped) % BUCKETS as u64).unwrap_or(0);
                self.outcomes[slot] = (0, 0);
            }
        }
        self.bucket = self.bucket.max(bucket);
        let slot = usize::try_from(self.bucket % BUCKETS as u64).unwrap_or(0);
        if ok {
            self.outcomes[slot].0 += 1;
        } else {
            self.outcomes[slot].1 += 1;
        }
    }

    fn totals(&self) -> (u64, u64) {
        self.outcomes.iter().fold((0, 0), |(ok, failed), (o, f)| {
            (ok + u64::from(*o), failed + u64::from(*f))
        })
    }
}

#[derive(Debug, Default)]
struct BreakerInner {
    window: Window,
    trial_successes: u16,
}

/// One server's breaker: closed, open until a time, then half-open with limited trials.
#[derive(Debug, Default)]
pub(super) struct Breaker {
    state: AtomicU8,
    open_until_unix_ms: AtomicU64,
    inner: Mutex<BreakerInner>,
}

impl Breaker {
    pub(super) fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    pub(super) fn is_half_open(&self) -> bool {
        self.state() == HALF_OPEN
    }

    /// Whether the server may be selected; an expired open period turns half-open on first look.
    pub(super) fn permits(&self, now_unix_ms: u64) -> bool {
        match self.state() {
            OPEN if now_unix_ms >= self.open_until_unix_ms.load(Ordering::Acquire) => {
                if self
                    .state
                    .compare_exchange(OPEN, HALF_OPEN, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.lock().trial_successes = 0;
                }
                true
            }
            OPEN => false,
            _ => true,
        }
    }

    /// Records one request outcome; `may_open` is false when opening would break `min_members`.
    pub(super) fn record(
        &self,
        ok: bool,
        now_unix_ms: u64,
        policy: CircuitBreakerPolicy,
        may_open: bool,
    ) -> Option<BreakerTransition> {
        let mut inner = self.lock();
        match self.state() {
            OPEN => None,
            HALF_OPEN if ok => {
                inner.trial_successes += 1;
                (inner.trial_successes >= policy.half_open_successes).then(|| {
                    inner.window = Window::default();
                    self.state.store(CLOSED, Ordering::Release);
                    BreakerTransition::Closed
                })
            }
            HALF_OPEN => may_open.then(|| self.open(&mut inner, now_unix_ms, policy)),
            _ => {
                inner.window.add(ok, now_unix_ms, policy);
                let (succeeded, failed) = inner.window.totals();
                let total = succeeded + failed;
                (total >= u64::from(policy.min_requests)
                    && failed * 100 >= u64::from(policy.failure_ratio_percent) * total
                    && may_open)
                    .then(|| self.open(&mut inner, now_unix_ms, policy))
            }
        }
    }

    fn open(
        &self,
        inner: &mut BreakerInner,
        now_unix_ms: u64,
        policy: CircuitBreakerPolicy,
    ) -> BreakerTransition {
        inner.window = Window::default();
        inner.trial_successes = 0;
        self.open_until_unix_ms.store(
            now_unix_ms.saturating_add(policy.open_ms),
            Ordering::Release,
        );
        self.state.store(OPEN, Ordering::Release);
        BreakerTransition::Opened
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BreakerInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CircuitBreakerPolicy {
    pub(super) const fn half_open_max(self) -> u64 {
        self.half_open_max as u64
    }
}
