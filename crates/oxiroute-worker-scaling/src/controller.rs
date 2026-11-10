use std::time::Duration;

/// What the active set is sized for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Workers that stay active however light the load.
    pub min_active: usize,
    /// Share of one worker's time, in permille, the active workers are kept under: more demand activates workers.
    pub target_busy_permille: u32,
    /// How long the load must fit in fewer workers before they are released.
    pub release_delay: Duration,
}

impl Policy {
    /// A worker is released only when the load fits the remaining ones at this share (percent) of the target, so the
    /// load that releases a worker is well under the load that activates one and the set does not flap.
    pub const RELEASE_PERCENT: u32 = 60;
}

/// Decides how many of the first `workers` workers are active from their busy shares, once per tick. It has no clock
/// of its own: the caller passes the time of each step, so tests drive it with a fake one.
#[derive(Debug)]
pub struct Controller {
    policy: Policy,
    workers: usize,
    active: usize,
    /// Demand smoothed over a few ticks; activation follows it, so one noisy tick does not activate a worker.
    smoothed: u32,
    /// Since when the load has fitted fewer workers than are active, and the most workers it needed since.
    release: Option<(Duration, usize)>,
}

impl Controller {
    /// Every worker starts active: a generation that starts under load serves it at full width, and releases workers
    /// once the load has proven light for the release delay.
    #[must_use]
    pub fn new(policy: Policy, workers: usize) -> Self {
        Self {
            policy,
            workers,
            active: workers,
            smoothed: 0,
            release: None,
        }
    }

    #[must_use]
    pub fn active(&self) -> usize {
        self.active
    }

    /// The active count after a tick at `now` in which worker `i` was busy `busy_permille[i]` of the time. The demand
    /// is the sum over all workers: connections that stayed on a released worker (TLS, HTTP/2) load it as well.
    pub fn step(&mut self, now: Duration, busy_permille: &[u32]) -> usize {
        let demand = busy_permille.iter().sum::<u32>();
        self.smoothed = self.smoothed.midpoint(demand);
        let wanted = self.workers_for(self.smoothed, self.policy.target_busy_permille);
        if wanted > self.active {
            self.active = wanted;
            self.release = None;
        } else {
            self.consider_release(now, demand);
        }
        self.active
    }

    fn workers_for(&self, demand: u32, share_permille: u32) -> usize {
        let share = share_permille.max(1);
        usize::try_from(demand.div_ceil(share))
            .unwrap_or(usize::MAX)
            .clamp(self.policy.min_active, self.workers)
    }

    /// Releases down to the workers the load has fitted for the whole delay. Any tick that needs as many workers as
    /// are active starts the wait over.
    fn consider_release(&mut self, now: Duration, demand: u32) {
        let band = self.policy.target_busy_permille * Policy::RELEASE_PERCENT / 100;
        let fits = self.workers_for(demand, band);
        if fits >= self.active {
            self.release = None;
            return;
        }
        let (since, most) = self.release.get_or_insert((now, fits));
        *most = (*most).max(fits);
        if now.saturating_sub(*since) >= self.policy.release_delay {
            self.active = *most;
            self.release = None;
        }
    }
}
