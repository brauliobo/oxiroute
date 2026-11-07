use std::{
    sync::{Arc, Mutex as StdMutex},
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
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{HealthFailure, RoundRobinPool, RuntimeEndpoint, routing::ServerHandle};

const MAX_CONCURRENT_PROBES: usize = 32;

#[derive(Clone)]
struct HealthTarget {
    check: Arc<dyn HealthCheck + Send + Sync>,
    down_interval: Duration,
    endpoint: RuntimeEndpoint,
    server: ServerHandle,
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
        if !self.server.checks_running() {
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
            &self.server,
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
            .resolve_server_addresses(&self.server)
            .await
            .map_err(|_| HealthFailure::ConnectFailed)?;
        self.run_probe_addresses(&addresses).await
    }

    fn next_interval(&self) -> Duration {
        if self.server.passively_ejected() {
            return self.fast_interval;
        }
        match self.server.state() {
            crate::EndpointHealthState::Unknown => self.fast_interval,
            crate::EndpointHealthState::Unhealthy => self.down_interval,
            crate::EndpointHealthState::Unchecked | crate::EndpointHealthState::Healthy => {
                self.interval
            }
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

/// What every target of one pool shares: its check, timing and pool.
#[derive(Clone)]
struct GroupTemplate {
    check: Arc<dyn HealthCheck + Send + Sync>,
    down_interval: Duration,
    fast_interval: Duration,
    healthy_threshold: u16,
    interval: Duration,
    pool: Arc<RoundRobinPool>,
    pool_name: String,
    timeout: Duration,
    unhealthy_threshold: u16,
}

impl GroupTemplate {
    fn target(&self, server: ServerHandle, endpoint: RuntimeEndpoint) -> HealthTarget {
        HealthTarget {
            check: Arc::clone(&self.check),
            down_interval: self.down_interval,
            endpoint,
            server,
            fast_interval: self.fast_interval,
            healthy_threshold: self.healthy_threshold,
            interval: self.interval,
            pool: Arc::clone(&self.pool),
            pool_name: self.pool_name.clone(),
            probe_lock: Arc::new(Mutex::new(())),
            timeout: self.timeout,
            unhealthy_threshold: self.unhealthy_threshold,
        }
    }
}

#[derive(Clone)]
pub(crate) struct HealthGroup {
    template: GroupTemplate,
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

/// Runs one probe loop per pool member. Members can be added and removed while it runs; each
/// target owns exactly one task, which stops when the target is removed or on shutdown.
#[derive(Clone)]
pub struct HealthSupervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    semaphore: Arc<Semaphore>,
    state: StdMutex<SupervisorState>,
}

struct SupervisorState {
    groups: Vec<GroupState>,
    running: Option<(tokio::runtime::Handle, ShutdownWatch)>,
}

struct GroupState {
    template: GroupTemplate,
    targets: Vec<RunningTarget>,
}

struct RunningTarget {
    target: HealthTarget,
    task: Option<JoinHandle<()>>,
}

impl SupervisorState {
    fn spawn(&self, target: &HealthTarget, semaphore: &Arc<Semaphore>) -> Option<JoinHandle<()>> {
        let (runtime, shutdown) = self.running.as_ref()?;
        Some(runtime.spawn(run_target(
            target.clone(),
            shutdown.clone(),
            Arc::clone(semaphore),
        )))
    }
}

impl HealthSupervisor {
    pub(crate) fn new(groups: Vec<HealthGroup>) -> Self {
        let groups = groups
            .into_iter()
            .map(|group| GroupState {
                template: group.template,
                targets: group
                    .targets
                    .into_iter()
                    .map(|target| RunningTarget { target, task: None })
                    .collect(),
            })
            .collect();
        Self {
            inner: Arc::new(SupervisorInner {
                semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
                state: StdMutex::new(SupervisorState {
                    groups,
                    running: None,
                }),
            }),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state().groups.is_empty()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SupervisorState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn probe_once(&self) {
        let targets = self
            .state()
            .groups
            .iter()
            .flat_map(|group| group.targets.iter().map(|running| running.target.clone()))
            .collect();
        run_targets(targets, Arc::clone(&self.inner.semaphore)).await;
    }

    /// Starts checking a server that joined `pool`. A pool without health checks ignores it.
    pub(crate) fn add_target(&self, pool: &str, server: ServerHandle, endpoint: RuntimeEndpoint) {
        let mut state = self.state();
        let Some(index) = state
            .groups
            .iter()
            .position(|group| group.template.pool_name == pool)
        else {
            return;
        };
        let target = state.groups[index].template.target(server, endpoint);
        let task = state.spawn(&target, &self.inner.semaphore);
        state.groups[index]
            .targets
            .push(RunningTarget { target, task });
    }

    /// Stops the checks of a server that left `pool`.
    pub(crate) fn remove_target(&self, pool: &str, server: &ServerHandle) {
        let mut state = self.state();
        for group in state
            .groups
            .iter_mut()
            .filter(|group| group.template.pool_name == pool)
        {
            group.targets.retain_mut(|running| {
                let keep = running.target.server != *server;
                if !keep && let Some(task) = running.task.take() {
                    task.abort();
                }
                keep
            });
        }
    }

    /// Runs every target until shutdown, then waits for their tasks.
    async fn run(&self, shutdown: ShutdownWatch) {
        {
            let mut state = self.state();
            let SupervisorState { groups, running } = &mut *state;
            let (runtime, watch) =
                running.insert((tokio::runtime::Handle::current(), shutdown.clone()));
            for member in groups.iter_mut().flat_map(|group| group.targets.iter_mut()) {
                member.task = Some(runtime.spawn(run_target(
                    member.target.clone(),
                    watch.clone(),
                    Arc::clone(&self.inner.semaphore),
                )));
            }
        }
        let mut shutdown = shutdown;
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
        let tasks = {
            let mut state = self.state();
            state.running = None;
            state
                .groups
                .iter_mut()
                .flat_map(|group| {
                    group
                        .targets
                        .iter_mut()
                        .filter_map(|running| running.task.take())
                })
                .collect::<Vec<_>>()
        };
        for task in tasks {
            let _ = task.await;
        }
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
        self.run(shutdown).await;
    }
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
    let template = GroupTemplate {
        check,
        down_interval: blueprint.down_interval,
        fast_interval: blueprint.fast_interval,
        healthy_threshold: blueprint.healthy_threshold,
        interval: blueprint.interval,
        pool: Arc::clone(pool),
        pool_name: name.to_owned(),
        timeout,
        unhealthy_threshold: blueprint.unhealthy_threshold,
    };
    let targets = pool
        .endpoints()
        .map(|(server, endpoint)| template.target(server, endpoint))
        .collect();

    HealthGroup { template, targets }
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
