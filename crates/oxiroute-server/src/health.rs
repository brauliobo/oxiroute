use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use log::info;
use oxiroute_config::{
    HealthCheck as HealthCheckConfig, HealthCheckType, HealthHttpVersion,
    validate_health_check_config,
};
use pingora::{
    ErrorType,
    http::RequestHeader,
    lb::{
        Backend,
        health_check::{HealthCheck, HttpHealthCheck, TcpHealthCheck},
    },
    server::ShutdownWatch,
    services::{ServiceReadyNotifier, background::BackgroundService},
};
use tokio::{
    sync::{Mutex, Semaphore},
    time::{sleep, timeout},
};

use crate::{HealthFailure, RoundRobinPool, RuntimeEndpoint};

const MAX_CONCURRENT_PROBES: usize = 32;

#[derive(Clone)]
struct HealthTarget {
    check: Arc<dyn HealthCheck + Send + Sync>,
    down_interval: Duration,
    endpoint: RuntimeEndpoint,
    endpoint_index: usize,
    fast_interval: Duration,
    healthy_threshold: u16,
    interval: Duration,
    pool: Arc<RoundRobinPool>,
    pool_name: String,
    probe_lock: Arc<Mutex<()>>,
    timeout: Duration,
    unhealthy_threshold: u16,
}

impl HealthTarget {
    async fn probe(self, semaphore: Arc<Semaphore>) {
        let _probe_guard = self.probe_lock.lock().await;
        if !self.pool.health_checks_running(self.endpoint_index) {
            return;
        }
        let Ok(_permit) = semaphore.acquire_owned().await else {
            return;
        };
        let result = timeout(self.timeout, self.run_probe()).await;
        let (healthy, failure) = match result {
            Ok(Ok(())) => (true, None),
            Err(_) => (false, Some(HealthFailure::Timeout)),
            Ok(Err(failure)) => (false, Some(failure)),
        };
        let transition = self.pool.record_health(
            self.endpoint_index,
            healthy,
            failure,
            unix_time_ms(),
            self.healthy_threshold,
            self.unhealthy_threshold,
        );
        if let Some((previous, next)) = transition {
            info!(
                "upstream pool `{}` endpoint {} health changed from {:?} to {:?}",
                self.pool_name, self.endpoint, previous, next
            );
        }
    }

    async fn run_probe(&self) -> Result<(), HealthFailure> {
        let addresses = self
            .pool
            .resolve_server_addresses(self.endpoint_index)
            .await
            .map_err(|_| HealthFailure::ConnectFailed)?;
        self.run_probe_addresses(&addresses).await
    }

    fn next_interval(&self) -> Duration {
        if self.pool.passive_ejected(self.endpoint_index) {
            return self.fast_interval;
        }
        match self.pool.health_state(self.endpoint_index) {
            Some(crate::EndpointHealthState::Unknown) => self.fast_interval,
            Some(crate::EndpointHealthState::Unhealthy) => self.down_interval,
            Some(crate::EndpointHealthState::Unchecked | crate::EndpointHealthState::Healthy)
            | None => self.interval,
        }
    }

    async fn run_probe_addresses(
        &self,
        addresses: &[std::net::SocketAddr],
    ) -> Result<(), HealthFailure> {
        let mut last_failure = HealthFailure::ConnectFailed;
        for address in addresses {
            let backend =
                Backend::new(&address.to_string()).map_err(|_| HealthFailure::ProtocolError)?;
            match self.check.check(&backend).await {
                Ok(()) => return Ok(()),
                Err(error) => last_failure = classify_probe_error(&error),
            }
        }
        Err(last_failure)
    }
}

fn classify_probe_error(error: &pingora::Error) -> HealthFailure {
    match error.etype() {
        ErrorType::ConnectTimedout
        | ErrorType::TLSHandshakeTimedout
        | ErrorType::ReadTimedout
        | ErrorType::WriteTimedout => HealthFailure::Timeout,
        ErrorType::ConnectRefused | ErrorType::ConnectNoRoute | ErrorType::ConnectError => {
            HealthFailure::ConnectFailed
        }
        ErrorType::CustomCode("non 200 code", _) => HealthFailure::UnexpectedStatus,
        _ => HealthFailure::ProtocolError,
    }
}

#[derive(Clone)]
pub(crate) struct HealthGroup {
    targets: Vec<HealthTarget>,
}

#[derive(Clone, Debug)]
pub(crate) struct HealthCheckBlueprint {
    kind: HealthCheckKindBlueprint,
    down_interval: Duration,
    fast_interval: Duration,
    healthy_threshold: u16,
    interval: Duration,
    pub(crate) startup: oxiroute_config::HealthStartup,
    timeout: Duration,
    unhealthy_threshold: u16,
}

#[derive(Clone, Debug)]
enum HealthCheckKindBlueprint {
    Tcp,
    Http {
        host: String,
        request: Box<RequestHeader>,
        expected_status: u16,
    },
}

impl HealthCheckBlueprint {
    pub(crate) fn compile(
        name: &str,
        config: &HealthCheckConfig,
    ) -> Result<Self, HealthBuildError> {
        validate_health_check_config(name, config)?;
        let kind = match config.kind {
            HealthCheckType::Tcp => HealthCheckKindBlueprint::Tcp,
            HealthCheckType::Http => {
                let host = config.host.clone().unwrap_or_default();
                let path = config
                    .path
                    .as_deref()
                    .ok_or(HealthBuildError::MissingPath)?;
                let version = match config.http_version.unwrap_or(HealthHttpVersion::Http11) {
                    HealthHttpVersion::Http10 => http::Version::HTTP_10,
                    HealthHttpVersion::Http11 => http::Version::HTTP_11,
                };
                let mut request = RequestHeader::build("GET", path.as_bytes(), Some(1))?;
                request.set_version(version);
                if !host.is_empty() {
                    request.append_header("Host", &host)?;
                }
                HealthCheckKindBlueprint::Http {
                    host,
                    request: Box::new(request),
                    expected_status: config.expected_status.unwrap_or(200),
                }
            }
        };
        Ok(Self {
            kind,
            down_interval: Duration::from_millis(
                config.down_interval_ms.unwrap_or(config.interval_ms),
            ),
            fast_interval: Duration::from_millis(
                config.fast_interval_ms.unwrap_or(config.interval_ms),
            ),
            healthy_threshold: config.healthy_threshold,
            interval: Duration::from_millis(config.interval_ms),
            startup: config.startup,
            timeout: Duration::from_millis(config.timeout_ms),
            unhealthy_threshold: config.unhealthy_threshold,
        })
    }
}

#[derive(Clone)]
pub struct HealthSupervisor {
    groups: Vec<HealthGroup>,
    semaphore: Arc<Semaphore>,
}

impl HealthSupervisor {
    pub(crate) fn new(groups: Vec<HealthGroup>) -> Self {
        Self {
            groups,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub async fn probe_once(&self) {
        run_targets(
            self.groups
                .iter()
                .flat_map(|group| group.targets.iter().cloned())
                .collect(),
            Arc::clone(&self.semaphore),
        )
        .await;
    }
}

#[async_trait]
impl BackgroundService for HealthSupervisor {
    async fn start_with_ready_notifier(
        &self,
        shutdown: ShutdownWatch,
        ready_notifier: ServiceReadyNotifier,
    ) {
        ready_notifier.notify_ready();
        stream::iter(self.groups.iter().cloned())
            .for_each_concurrent(None, |group| {
                run_group(group, shutdown.clone(), Arc::clone(&self.semaphore))
            })
            .await;
    }
}

async fn run_group(group: HealthGroup, shutdown: ShutdownWatch, semaphore: Arc<Semaphore>) {
    stream::iter(group.targets)
        .for_each_concurrent(None, |target| {
            run_target(target, shutdown.clone(), Arc::clone(&semaphore))
        })
        .await;
}

async fn run_target(target: HealthTarget, mut shutdown: ShutdownWatch, semaphore: Arc<Semaphore>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            () = target.clone().probe(Arc::clone(&semaphore)) => {}
            _ = shutdown.changed() => return,
        }
        if *shutdown.borrow() {
            return;
        }
        let interval = target.next_interval();
        tokio::select! {
            () = sleep(interval) => {}
            _ = shutdown.changed() => return,
        }
    }
}

async fn run_targets(targets: Vec<HealthTarget>, semaphore: Arc<Semaphore>) {
    stream::iter(targets)
        .for_each_concurrent(None, |target| target.probe(Arc::clone(&semaphore)))
        .await;
}

pub(crate) fn acquire_health_group(
    name: &str,
    pool: &Arc<RoundRobinPool>,
    blueprint: &HealthCheckBlueprint,
) -> HealthGroup {
    let timeout = blueprint.timeout;
    let check: Arc<dyn HealthCheck + Send + Sync> = match &blueprint.kind {
        HealthCheckKindBlueprint::Tcp => {
            let mut check = TcpHealthCheck::default();
            check.peer_template.options.connection_timeout = Some(timeout);
            check.peer_template.options.total_connection_timeout = Some(timeout);
            Arc::new(check)
        }
        HealthCheckKindBlueprint::Http {
            host,
            request,
            expected_status,
        } => {
            let mut check = HttpHealthCheck::new(host, false);
            check.req = (**request).clone();
            let expected_status = *expected_status;
            check.validator = Some(Box::new(move |response| {
                let actual = response.status.as_u16();
                if actual == expected_status {
                    Ok(())
                } else {
                    pingora::Error::e_explain(
                        ErrorType::CustomCode("non 200 code", actual),
                        format!(
                            "health check expected status {expected_status}, received {actual}"
                        ),
                    )
                }
            }));
            check.peer_template.options.connection_timeout = Some(timeout);
            check.peer_template.options.total_connection_timeout = Some(timeout);
            check.peer_template.options.read_timeout = Some(timeout);
            check.peer_template.options.write_timeout = Some(timeout);
            Arc::new(check)
        }
    };
    let targets = pool
        .endpoints()
        .map(|(endpoint_index, endpoint)| HealthTarget {
            check: Arc::clone(&check),
            down_interval: blueprint.down_interval,
            endpoint,
            endpoint_index,
            fast_interval: blueprint.fast_interval,
            healthy_threshold: blueprint.healthy_threshold,
            interval: blueprint.interval,
            pool: Arc::clone(pool),
            pool_name: name.to_owned(),
            probe_lock: Arc::new(Mutex::new(())),
            timeout,
            unhealthy_threshold: blueprint.unhealthy_threshold,
        })
        .collect();

    HealthGroup { targets }
}

#[derive(Debug, thiserror::Error)]
pub enum HealthBuildError {
    #[error("invalid health check configuration: {0}")]
    InvalidConfig(#[from] oxiroute_config::ConfigError),
    #[error("HTTP health check requires a path")]
    MissingPath,
    #[error("health check construction failed: {0}")]
    Pingora(#[from] Box<pingora::Error>),
}

fn unix_time_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests;
