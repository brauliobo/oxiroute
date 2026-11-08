use std::collections::{HashMap, HashSet};

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
    /// Per route, the average of each server by name; routes are bounded by the configuration.
    routes: HashMap<String, HashMap<String, Sample>>,
}

impl Observations {
    /// The average of `server` on `route`, created on its first response.
    fn sample(&mut self, route: &str, server: &str) -> &mut Sample {
        if !self.routes.contains_key(route) {
            self.routes.insert(route.to_owned(), HashMap::new());
        }
        let samples = self.routes.get_mut(route).expect("route inserted above");
        if !samples.contains_key(server) {
            samples.insert(server.to_owned(), Sample::default());
        }
        samples.get_mut(server).expect("server inserted above")
    }
}

/// Moving average of the time to first byte per route and server, judged once per interval.
#[derive(Debug, Default)]
pub(super) struct OutlierDetector {
    observations: Mutex<Observations>,
}

impl OutlierDetector {
    /// Records one response time. When the interval ended, returns the outliers among the servers
    /// `members` lists, worst first, and forgets servers that left the pool.
    pub(super) fn record<'a>(
        &self,
        route: &str,
        server: &str,
        members: impl FnOnce() -> Vec<&'a str>,
        elapsed_ms: f64,
        now_unix_ms: u64,
        policy: OutlierPolicy,
    ) -> Vec<String> {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sample = observations.sample(route, server);
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
        let members = members().into_iter().collect::<HashSet<_>>();
        let mut outliers = observations
            .routes
            .values_mut()
            .flat_map(|samples| {
                samples.retain(|name, _| members.contains(name.as_str()));
                judge(samples, policy)
            })
            .collect::<Vec<_>>();
        outliers.sort_by(|a, b| b.0.total_cmp(&a.0));
        outliers.into_iter().map(|(_, server)| server).collect()
    }

    /// Forgets a server's averages, so an ejected server is judged afresh after it returns.
    pub(super) fn forget(&self, server: &str) {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for samples in observations.routes.values_mut() {
            samples.remove(server);
        }
    }
}

/// The servers of one route above `factor` times the median of their peers, and the start of a
/// new window. The peers of a server are all the other judged servers; sorting the averages once
/// gives each server its peer median without sorting again.
fn judge(samples: &mut HashMap<String, Sample>, policy: OutlierPolicy) -> Vec<(f64, String)> {
    let judged = samples
        .iter()
        .filter(|(_, sample)| sample.count >= policy.min_samples)
        .map(|(name, sample)| (name, sample.ewma_ms))
        .collect::<Vec<_>>();
    let mut sorted = judged.iter().map(|(_, ewma)| *ewma).collect::<Vec<_>>();
    sorted.sort_by(f64::total_cmp);
    let outliers = judged
        .iter()
        .filter_map(|(name, ewma)| {
            let own = sorted.partition_point(|value| value.total_cmp(ewma).is_lt());
            let median = median_without(&sorted, own)?;
            let ratio = ewma / median.max(f64::EPSILON);
            (ratio * 100.0 > f64::from(policy.factor_percent)).then(|| (ratio, (*name).clone()))
        })
        .collect();
    for sample in samples.values_mut() {
        sample.count = 0;
    }
    outliers
}

/// The median of the sorted `values` without the one at `skipped`.
fn median_without(values: &[f64], skipped: usize) -> Option<f64> {
    let peers = values.len().checked_sub(1).filter(|peers| *peers > 0)?;
    let peer = |index: usize| values[index + usize::from(index >= skipped)];
    let middle = peers / 2;
    Some(if peers % 2 == 1 {
        peer(middle)
    } else {
        f64::midpoint(peer(middle - 1), peer(middle))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> OutlierPolicy {
        OutlierPolicy {
            factor_percent: 150,
            min_samples: 2,
            interval_ms: 1_000,
            max_ejected_percent: 50,
        }
    }

    /// Every server against the median of a freshly sorted list of its peers.
    fn per_server_medians(samples: &HashMap<String, Sample>) -> Vec<(f64, String)> {
        let judged = samples
            .iter()
            .filter(|(_, sample)| sample.count >= policy().min_samples)
            .map(|(name, sample)| (name.clone(), sample.ewma_ms))
            .collect::<Vec<_>>();
        judged
            .iter()
            .filter_map(|(name, ewma)| {
                let mut peers = judged
                    .iter()
                    .filter(|(other, _)| other != name)
                    .map(|(_, ewma)| *ewma)
                    .collect::<Vec<_>>();
                peers.sort_by(f64::total_cmp);
                let middle = peers.len() / 2;
                let median = match peers.len() {
                    0 => return None,
                    length if length % 2 == 1 => peers[middle],
                    _ => f64::midpoint(peers[middle - 1], peers[middle]),
                };
                let ratio = ewma / median.max(f64::EPSILON);
                (ratio * 100.0 > 150.0).then(|| (ratio, name.clone()))
            })
            .collect()
    }

    #[test]
    fn judging_with_one_sort_finds_the_outliers_of_the_peer_medians() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for servers in 1..40_u64 {
            let mut samples = (0..servers)
                .map(|index| {
                    let ewma_ms = f64::from(u32::try_from(next() % 12).expect("small") + 1) * 5.0;
                    let count = u32::try_from(next() % 4).expect("small");
                    (format!("s{index}"), Sample { ewma_ms, count })
                })
                .collect::<HashMap<_, _>>();
            let mut expected = per_server_medians(&samples);

            let mut judged = judge(&mut samples, policy());

            expected.sort_by(|a, b| a.1.cmp(&b.1));
            judged.sort_by(|a, b| a.1.cmp(&b.1));
            assert_eq!(judged, expected, "{servers} servers");
            assert!(samples.values().all(|sample| sample.count == 0));
        }
    }
}
