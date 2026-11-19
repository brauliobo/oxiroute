use super::{Baseline, Clock, Window};
use crate::{
    cli::{
        top::{
            model::{Cell, Tone},
            source::Sample,
        },
        units,
    },
    observability::stats_page::model::{
        Histogram, ListenerStats, PoolStats, ServerStats, StatsSnapshot,
    },
};

pub(crate) struct Sampling<'a> {
    pub(crate) now: &'a Sample,
    pub(crate) before: Option<&'a Sample>,
    pub(crate) window: Window,
    pub(crate) complete: bool,
    clock: Clock,
}

impl<'a> Sampling<'a> {
    pub(crate) fn new(now: &'a Sample, baseline: Option<Baseline<'a>>, window: Window) -> Self {
        let stats_now = now.snapshot.sampled_at_unix_ms;
        let clock = match (window, &baseline) {
            (Window::Life, _) => Clock::life(stats_now, now.snapshot.process.uptime_ms),
            (_, Some(base)) => Clock::delta(stats_now, base.sample.snapshot.sampled_at_unix_ms),
            (_, None) => Clock::filling(),
        };
        let (before, complete) = match baseline {
            Some(base) => (Some(base.sample), base.complete),
            None => (None, window == Window::Life),
        };
        Self {
            now,
            before,
            window,
            complete,
            clock,
        }
    }

    pub(crate) const fn stats(&self) -> &'a StatsSnapshot {
        &self.now.snapshot
    }

    pub(crate) fn before_stats(&self) -> Option<&'a StatsSnapshot> {
        self.before.map(|before| &before.snapshot)
    }

    pub(crate) const fn windowed(&self) -> bool {
        self.clock.windowed()
    }

    pub(crate) fn rate(&self, now: u64, before: Option<u64>) -> Option<f64> {
        self.clock.rate(now, before, None)
    }

    pub(crate) fn feed_clock(&self, now_ms: u64, before_ms: Option<u64>) -> Clock {
        match (self.window, before_ms) {
            (Window::Life, _) => Clock::life(now_ms, self.now.snapshot.process.uptime_ms),
            (_, Some(before_ms)) => Clock::delta(now_ms, before_ms),
            (_, None) => Clock::filling(),
        }
    }

    pub(crate) fn listener(&self, name: &str) -> Option<&'a ListenerStats> {
        self.before_stats()?
            .listeners
            .iter()
            .find(|listener| listener.name == name)
    }

    pub(crate) fn pool(&self, name: &str) -> Option<&'a PoolStats> {
        self.before_stats()?
            .pools
            .iter()
            .find(|pool| pool.name == name)
    }

    pub(crate) fn server(&self, pool: &str, name: &str) -> Option<&'a ServerStats> {
        self.pool(pool)?
            .servers
            .iter()
            .find(|server| server.name == name)
    }

    pub(crate) fn percentiles(&self, now: &Histogram, before: Option<&Histogram>) -> [Cell; 2] {
        let bounds = &self.stats().bucket_bounds_ms.operation;
        [0.50, 0.99].map(|quantile| {
            let observed = if self.windowed() {
                before.and_then(|before| window(now, before))
            } else {
                Some(lifetime(now))
            };
            observed
                .and_then(|histogram| histogram.quantile(quantile, bounds))
                .map_or_else(Cell::missing, Bound::cell)
        })
    }

    pub(crate) fn ratio(&self, (hits, misses): (u64, u64), before: Option<(u64, u64)>) -> Cell {
        let (hits, misses) = match (self.windowed(), before) {
            (false, _) => (hits, misses),
            (true, Some((before_hits, before_misses))) => {
                match (
                    hits.checked_sub(before_hits),
                    misses.checked_sub(before_misses),
                ) {
                    (Some(hits), Some(misses)) => (hits, misses),
                    _ => return Cell::missing(),
                }
            }
            (true, None) => return Cell::missing(),
        };
        Cell::percent(units::percent(hits, hits + misses))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Bound {
    AtMost(u64),
    Over(u64),
}

impl Bound {
    fn cell(self) -> Cell {
        match self {
            Self::AtMost(ms) => Cell::millis(ms),
            Self::Over(ms) => Cell::over(ms).toned(Tone::Warn),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Observed {
    count: u64,
    buckets: Vec<u64>,
}

impl Observed {
    pub(crate) fn quantile(&self, quantile: f64, bounds: &[u64]) -> Option<Bound> {
        if self.count == 0 {
            return None;
        }
        let rank = (units::as_f64(self.count) * quantile).ceil().max(1.0);
        let at = self
            .buckets
            .iter()
            .position(|cumulative| units::as_f64(*cumulative) >= rank);
        match at {
            Some(index) => bounds.get(index).copied().map(Bound::AtMost),
            None => bounds.last().copied().map(Bound::Over),
        }
    }
}

pub(crate) fn lifetime(histogram: &Histogram) -> Observed {
    Observed {
        count: histogram.count.get(),
        buckets: histogram
            .buckets
            .iter()
            .map(|bucket| bucket.get())
            .collect(),
    }
}

pub(crate) fn window(now: &Histogram, before: &Histogram) -> Option<Observed> {
    let buckets = now
        .buckets
        .iter()
        .zip(&before.buckets)
        .map(|(now, before)| now.get().checked_sub(before.get()))
        .collect::<Option<Vec<_>>>()?;
    Some(Observed {
        count: now.count.get().checked_sub(before.count.get())?,
        buckets,
    })
}
