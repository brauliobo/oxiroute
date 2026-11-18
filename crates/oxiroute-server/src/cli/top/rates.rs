use super::rows::{Cell, Tone};
use crate::cli::units;
use crate::observability::stats_page::model::{
    Histogram, ListenerStats, PoolStats, ServerStats, StatsSnapshot,
};

pub(super) struct Sampling<'a> {
    pub(super) now: &'a StatsSnapshot,
    pub(super) before: Option<&'a StatsSnapshot>,
    seconds: f64,
}

impl<'a> Sampling<'a> {
    pub(super) fn new(now: &'a StatsSnapshot, before: Option<&'a StatsSnapshot>) -> Self {
        let before = before.filter(|before| {
            before.sampled_at_unix_ms < now.sampled_at_unix_ms
                && before.process.uptime_ms <= now.process.uptime_ms
        });
        let elapsed_ms = before.map_or(0, |before| {
            now.sampled_at_unix_ms - before.sampled_at_unix_ms
        });
        Self {
            now,
            before,
            seconds: units::as_f64(elapsed_ms) / 1_000.0,
        }
    }

    pub(super) fn windowed(&self) -> bool {
        self.before.is_some()
    }

    pub(super) fn rate(&self, now: u64, before: Option<u64>) -> Option<f64> {
        let delta = now.checked_sub(before?)?;
        Some(units::as_f64(delta) / self.seconds)
    }

    pub(super) fn listener(&self, name: &str) -> Option<&'a ListenerStats> {
        self.before?
            .listeners
            .iter()
            .find(|listener| listener.name == name)
    }

    pub(super) fn pool(&self, name: &str) -> Option<&'a PoolStats> {
        self.before?.pools.iter().find(|pool| pool.name == name)
    }

    pub(super) fn server(&self, pool: &str, name: &str) -> Option<&'a ServerStats> {
        self.pool(pool)?
            .servers
            .iter()
            .find(|server| server.name == name)
    }

    pub(super) fn percentiles(&self, now: &Histogram, before: Option<&Histogram>) -> [Cell; 2] {
        let bounds = &self.now.bucket_bounds_ms.operation;
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

    pub(super) fn ratio(&self, (hits, misses): (u64, u64), before: Option<(u64, u64)>) -> Cell {
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
        Cell::measured(units::percent(hits, hits + misses), |value| {
            format!("{value:.0}%")
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Bound {
    AtMost(u64),
    Over(u64),
}

impl Bound {
    fn cell(self) -> Cell {
        match self {
            Self::AtMost(ms) => Cell::number(units::millis(ms), units::as_f64(ms)),
            Self::Over(ms) => {
                Cell::number(format!(">{}", units::millis(ms)), units::as_f64(ms) + 1.0)
                    .toned(Tone::Warn)
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Observed {
    count: u64,
    buckets: Vec<u64>,
}

impl Observed {
    pub(super) fn quantile(&self, quantile: f64, bounds: &[u64]) -> Option<Bound> {
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

pub(super) fn lifetime(histogram: &Histogram) -> Observed {
    Observed {
        count: histogram.count.get(),
        buckets: histogram
            .buckets
            .iter()
            .map(|bucket| bucket.get())
            .collect(),
    }
}

pub(super) fn window(now: &Histogram, before: &Histogram) -> Option<Observed> {
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
