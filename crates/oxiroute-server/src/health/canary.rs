//! The per-pool synthetic request: a `GET` through the pool's own server selection, as a client
//! reaches it, whose result is kept in the pool for metrics, the status page and `oxiroute check`.

use std::time::Instant;

use oxiroute_config::{HealthCheckType, HealthStartup, PoolCanary};
use tokio::time::timeout;

use super::{
    Arc, Duration, HealthBuildError, HealthCheck, HealthCheckBlueprint, HealthCheckConfig,
    HealthFailure, RoundRobinPool, build_check, probe_addresses,
};
use crate::routing::{CanaryResult, CanaryRun, CanarySelection, CanaryState};

#[cfg(test)]
mod tests;

/// A pool's canary as compiled from its configuration, before it has a pool to run against.
#[derive(Clone)]
pub(crate) struct CanaryBlueprint {
    check: Arc<dyn HealthCheck + Send + Sync>,
    interval: Duration,
    timeout: Duration,
    max_latency: Option<Duration>,
}

impl CanaryBlueprint {
    /// # Errors
    ///
    /// Returns an error when the request cannot be built from the configured path or host.
    pub(crate) fn compile(config: &PoolCanary) -> Result<Self, HealthBuildError> {
        let blueprint = HealthCheckBlueprint::build(&HealthCheckConfig {
            kind: HealthCheckType::Http,
            interval_ms: config.interval_ms,
            timeout_ms: config.timeout_ms,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup: HealthStartup::default(),
            fast_interval_ms: None,
            down_interval_ms: None,
            host: config.host.clone(),
            path: Some(config.path.clone()),
            expected_status: Some(config.expected_status),
            http_version: None,
        })?;
        Ok(Self {
            check: build_check(&blueprint),
            interval: Duration::from_millis(config.interval_ms),
            timeout: Duration::from_millis(config.timeout_ms),
            max_latency: config.max_latency_ms.map(Duration::from_millis),
        })
    }
}

#[derive(Clone)]
pub(crate) struct CanaryRunner {
    pool: Arc<RoundRobinPool>,
    state: Arc<CanaryState>,
    plan: CanaryBlueprint,
}

impl CanaryRunner {
    /// Attaches a result state to `pool` and runs `plan` against it.
    pub(crate) fn new(pool: Arc<RoundRobinPool>, plan: &CanaryBlueprint) -> Self {
        let state = Arc::new(CanaryState::new(plan.interval));
        pool.attach_canary(Arc::clone(&state));
        Self {
            pool,
            state,
            plan: plan.clone(),
        }
    }

    pub(super) const fn interval(&self) -> Duration {
        self.plan.interval
    }

    /// Sends one canary request and records how it went.
    pub(crate) async fn run_once(&self) {
        let started = Instant::now();
        let (result, reason, server) = match self.pool.canary_selection() {
            CanarySelection::Unavailable => (CanaryResult::Unavailable, None, None),
            CanarySelection::Busy => (CanaryResult::Busy, None, None),
            CanarySelection::Leased(lease) => {
                let probe = async {
                    let addresses = lease
                        .resolve_addresses()
                        .await
                        .map_err(|_| HealthFailure::ConnectFailed)?;
                    probe_addresses(self.plan.check.as_ref(), &addresses).await
                };
                let outcome = timeout(self.plan.timeout, probe)
                    .await
                    .unwrap_or(Err(HealthFailure::Timeout));
                let server = Some(lease.server_name().to_owned());
                match outcome {
                    Err(reason) => (CanaryResult::Failed, Some(reason), server),
                    Ok(())
                        if self
                            .plan
                            .max_latency
                            .is_some_and(|limit| started.elapsed() > limit) =>
                    {
                        (CanaryResult::Slow, None, server)
                    }
                    Ok(()) => (CanaryResult::Ok, None, server),
                }
            }
        };
        let run = CanaryRun {
            result,
            reason,
            server,
            duration: started.elapsed(),
        };
        let reason = run.reason.map_or("unknown", HealthFailure::as_str);
        match self.state.complete(run) {
            Some((_, CanaryResult::Failed)) => {
                log::warn!("pool `{}` canary failed: {reason}", self.pool.name());
            }
            Some((_, CanaryResult::Ok | CanaryResult::Slow)) => {
                log::info!("pool `{}` canary recovered", self.pool.name());
            }
            _ => {}
        }
    }
}
