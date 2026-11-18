#[allow(clippy::wildcard_imports)]
use super::*;
use std::num::NonZeroU64;

pub(super) const fn carried(value: u64) -> Option<NonZeroU64> {
    NonZeroU64::new(value)
}

pub(super) const fn restored(value: Option<NonZeroU64>) -> u64 {
    match value {
        Some(value) => value.get(),
        None => 0,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerHealthCarry {
    pub server: String,
    pub identity: u64,
    pub state: EndpointHealthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitioned_at: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successes: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<HealthFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive: Option<PassiveCarry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker: Option<BreakerCarry>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PassiveCarry {
    #[serde(default)]
    pub ejected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_count: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ejection_count: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_step: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<HealthFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ejected_at: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ejected_until: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_count: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_at: Option<NonZeroU64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BreakerCarry {
    pub half_open: bool,
    pub open_until: u64,
    pub trial_successes: u16,
}

impl ServerHealthCarry {
    #[must_use]
    pub fn significant(&self) -> bool {
        self.state == EndpointHealthState::Unhealthy
            || self.failures.is_some()
            || self.breaker.is_some()
            || self.passive.as_ref().is_some_and(|passive| passive.ejected)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PoolHealthCarry {
    pub pool: String,
    pub servers: Vec<ServerHealthCarry>,
}

#[must_use]
pub fn fit_health_carry(
    carried: Vec<PoolHealthCarry>,
    budget: usize,
) -> (Vec<PoolHealthCarry>, usize) {
    const POOL_OVERHEAD: usize = 32;
    let mut candidates = carried
        .iter()
        .enumerate()
        .flat_map(|(pool, carry)| carry.servers.iter().map(move |server| (pool, server)))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, server)| !server.significant());
    let mut kept = vec![Vec::new(); carried.len()];
    let mut used = 0_usize;
    let mut left_out = 0;
    for (pool, server) in candidates {
        let size = serde_json::to_vec(server).map_or(usize::MAX, |json| json.len() + 1);
        let overhead = if kept[pool].is_empty() {
            POOL_OVERHEAD + carried[pool].pool.len()
        } else {
            0
        };
        if used.saturating_add(size).saturating_add(overhead) > budget {
            left_out += 1;
            continue;
        }
        used += size + overhead;
        kept[pool].push(server.clone());
    }
    let fitted = carried
        .into_iter()
        .zip(kept)
        .filter(|(_, servers)| !servers.is_empty())
        .map(|(carry, mut servers)| {
            let order = |name: &str| {
                carry
                    .servers
                    .iter()
                    .position(|server| server.server == name)
            };
            servers.sort_by_key(|server| order(&server.server));
            PoolHealthCarry {
                pool: carry.pool,
                servers,
            }
        })
        .collect();
    (fitted, left_out)
}

impl PoolHealthState {
    pub(super) fn restore(&self, server: &PoolEndpoint, carried: &ServerHealthCarry) -> bool {
        if server.identity() != carried.identity {
            return false;
        }
        let _writer = self
            .health_writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.health_version.fetch_add(1, Ordering::AcqRel);
        server.restore_health(carried);
        self.health_version.fetch_add(1, Ordering::Release);
        true
    }
}
