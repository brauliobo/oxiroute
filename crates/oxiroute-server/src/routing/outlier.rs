use std::collections::HashMap;

#[allow(clippy::wildcard_imports)]
use super::*;

/// Weight of a new response time in a server's moving average.
const EWMA_WEIGHT: f64 = 0.2;

/// Latency outlier settings of a pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutlierPolicy {
    factor_percent: u32,
    min_samples: u32,
    interval_ms: u64,
    pub(super) max_ejected_percent: u8,
}

impl From<&OutlierDetection> for OutlierPolicy {
    fn from(config: &OutlierDetection) -> Self {
        Self {
            factor_percent: config.factor_percent,
            min_samples: config.min_samples,
            interval_ms: config.interval_ms,
            max_ejected_percent: config.max_ejected_percent,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    ewma_ms: f64,
    count: u32,
}

#[derive(Debug, Default)]
struct Observations {
    evaluated_at_ms: u64,
    /// Per route, one sample per server in pool order; routes are bounded by the configuration.
    routes: HashMap<String, Vec<Sample>>,
}

/// Moving average of the time to first byte per route and server, judged once per interval.
#[derive(Debug, Default)]
pub(super) struct OutlierDetector {
    observations: Mutex<Observations>,
}

impl OutlierDetector {
    /// Records one response time and returns the servers found to be outliers when the interval ended.
    pub(super) fn record(
        &self,
        route: &str,
        server: usize,
        servers: usize,
        elapsed_ms: f64,
        now_unix_ms: u64,
        policy: OutlierPolicy,
    ) -> Vec<usize> {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let samples = observations
            .routes
            .entry(route.to_owned())
            .or_insert_with(|| vec![Sample::default(); servers]);
        let sample = &mut samples[server];
        sample.ewma_ms = if sample.count == 0 && sample.ewma_ms == 0.0 {
            elapsed_ms
        } else {
            sample
                .ewma_ms
                .mul_add(1.0 - EWMA_WEIGHT, elapsed_ms * EWMA_WEIGHT)
        };
        sample.count = sample.count.saturating_add(1);
        if observations.evaluated_at_ms == 0 {
            observations.evaluated_at_ms = now_unix_ms;
        }
        if now_unix_ms.saturating_sub(observations.evaluated_at_ms) < policy.interval_ms {
            return Vec::new();
        }
        observations.evaluated_at_ms = now_unix_ms;
        let outliers = observations
            .routes
            .values_mut()
            .flat_map(|samples| judge(samples, policy))
            .collect::<Vec<_>>();
        outliers.into_iter().map(|(_, server)| server).collect()
    }

    /// Forgets a server's averages, so an ejected server is judged afresh after it returns.
    pub(super) fn forget(&self, server: usize) {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for samples in observations.routes.values_mut() {
            samples[server] = Sample::default();
        }
    }
}

/// The servers of one route above `factor` times the median of their peers, worst first, and the
/// start of a new window.
fn judge(samples: &mut [Sample], policy: OutlierPolicy) -> Vec<(f64, usize)> {
    let judged = samples
        .iter()
        .enumerate()
        .filter(|(_, sample)| sample.count >= policy.min_samples)
        .map(|(index, sample)| (index, sample.ewma_ms))
        .collect::<Vec<_>>();
    let mut outliers = judged
        .iter()
        .filter_map(|(index, ewma)| {
            let mut peers = judged
                .iter()
                .filter(|(other, _)| other != index)
                .map(|(_, ewma)| *ewma)
                .collect::<Vec<_>>();
            let median = median(&mut peers)?;
            let ratio = ewma / median.max(f64::EPSILON);
            (ratio * 100.0 > f64::from(policy.factor_percent)).then_some((ratio, *index))
        })
        .collect::<Vec<_>>();
    outliers.sort_by(|a, b| b.0.total_cmp(&a.0));
    for sample in samples {
        sample.count = 0;
    }
    outliers
}

fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    match values.len() {
        0 => None,
        length if length % 2 == 1 => Some(values[middle]),
        _ => Some(f64::midpoint(values[middle - 1], values[middle])),
    }
}
