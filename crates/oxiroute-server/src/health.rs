use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use log::info;
use oxiroute_config::{
    HealthCheck as HealthCheckConfig, HealthCheckType, validate_health_check_config,
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
    endpoint: RuntimeEndpoint,
    endpoint_index: usize,
    healthy_threshold: u16,
    pool: Arc<RoundRobinPool>,
    pool_name: String,
    probe_lock: Arc<Mutex<()>>,
    timeout: Duration,
    unhealthy_threshold: u16,
}

impl HealthTarget {
    async fn probe(self, semaphore: Arc<Semaphore>) {
        let _probe_guard = self.probe_lock.lock().await;
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
            .endpoint
            .resolve_addresses()
            .await
            .map_err(|_| HealthFailure::ConnectFailed)?;
        self.run_probe_addresses(&addresses).await
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
    interval: Duration,
    targets: Vec<HealthTarget>,
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
            run_target(
                target,
                group.interval,
                shutdown.clone(),
                Arc::clone(&semaphore),
            )
        })
        .await;
}

async fn run_target(
    target: HealthTarget,
    interval: Duration,
    mut shutdown: ShutdownWatch,
    semaphore: Arc<Semaphore>,
) {
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

pub(crate) fn compile_health_group(
    name: &str,
    pool: &Arc<RoundRobinPool>,
    config: &HealthCheckConfig,
) -> Result<HealthGroup, HealthBuildError> {
    validate_health_check_config(name, config)?;
    let timeout = Duration::from_millis(config.timeout_ms);
    let check: Arc<dyn HealthCheck + Send + Sync> = match config.kind {
        HealthCheckType::Tcp => {
            let mut check = TcpHealthCheck::default();
            check.peer_template.options.connection_timeout = Some(timeout);
            check.peer_template.options.total_connection_timeout = Some(timeout);
            Arc::new(check)
        }
        HealthCheckType::Http => {
            let host = config
                .host
                .as_deref()
                .ok_or(HealthBuildError::MissingHost)?;
            let path = config
                .path
                .as_deref()
                .ok_or(HealthBuildError::MissingPath)?;
            let mut request = RequestHeader::build("GET", path.as_bytes(), Some(1))?;
            request.append_header("Host", host)?;
            let mut check = HttpHealthCheck::new(host, false);
            check.req = request;
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
            endpoint,
            endpoint_index,
            healthy_threshold: config.healthy_threshold,
            pool: Arc::clone(pool),
            pool_name: name.to_owned(),
            probe_lock: Arc::new(Mutex::new(())),
            timeout,
            unhealthy_threshold: config.unhealthy_threshold,
        })
        .collect();

    Ok(HealthGroup {
        interval: Duration::from_millis(config.interval_ms),
        targets,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum HealthBuildError {
    #[error("invalid health check configuration: {0}")]
    InvalidConfig(#[from] oxiroute_config::ConfigError),
    #[error("HTTP health check requires a host")]
    MissingHost,
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
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    struct BlockingCheck {
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        release: Semaphore,
        started: AtomicUsize,
    }

    struct AddressFallbackCheck {
        attempts: StdMutex<Vec<std::net::SocketAddr>>,
        healthy: std::net::SocketAddr,
    }

    #[async_trait]
    impl HealthCheck for AddressFallbackCheck {
        async fn check(&self, target: &Backend) -> pingora::Result<()> {
            let address = *target.addr.as_inet().expect("inet health target");
            self.attempts.lock().expect("health attempts").push(address);
            if address == self.healthy {
                Ok(())
            } else {
                Err(pingora::Error::new_in(ErrorType::ConnectRefused))
            }
        }

        fn health_threshold(&self, _success: bool) -> usize {
            1
        }
    }

    #[tokio::test]
    async fn health_falls_back_to_the_second_resolved_address() {
        let first = std::net::SocketAddr::from(([192, 0, 2, 1], 443));
        let second = std::net::SocketAddr::from(([192, 0, 2, 2], 443));
        let endpoint = RuntimeEndpoint::Dns {
            host: "origin.example.test".into(),
            port: 443,
        };
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "fallback".into(),
                [endpoint.clone()],
                oxiroute_config::UpstreamAlgorithm::RoundRobin,
                true,
            )
            .expect("health pool"),
        );
        let check = Arc::new(AddressFallbackCheck {
            attempts: StdMutex::new(Vec::new()),
            healthy: second,
        });
        let target = HealthTarget {
            check: Arc::<AddressFallbackCheck>::clone(&check),
            endpoint,
            endpoint_index: 0,
            healthy_threshold: 1,
            pool,
            pool_name: "fallback".into(),
            probe_lock: Arc::new(Mutex::new(())),
            timeout: Duration::from_secs(1),
            unhealthy_threshold: 1,
        };

        target
            .run_probe_addresses(&[first, second])
            .await
            .expect("second address is healthy");

        assert_eq!(
            *check.attempts.lock().expect("health attempts"),
            vec![first, second]
        );
    }

    #[async_trait]
    impl HealthCheck for BlockingCheck {
        async fn check(&self, _target: &Backend) -> pingora::Result<()> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active.fetch_max(active, Ordering::SeqCst);
            self.started.fetch_add(1, Ordering::SeqCst);
            self.release
                .acquire()
                .await
                .expect("release semaphore")
                .forget();
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        fn health_threshold(&self, _success: bool) -> usize {
            1
        }
    }

    #[tokio::test]
    async fn probes_share_a_global_concurrency_bound() {
        let addresses: Vec<std::net::SocketAddr> = (10_000..10_033)
            .map(|port| ([127, 0, 0, 1], port).into())
            .collect::<Vec<_>>();
        let pool = Arc::new(
            RoundRobinPool::new_named(
                "bounded".into(),
                addresses.iter().copied().map(RuntimeEndpoint::from),
                oxiroute_config::UpstreamAlgorithm::RoundRobin,
                true,
            )
            .expect("health pool"),
        );
        let check = Arc::new(BlockingCheck {
            active: AtomicUsize::new(0),
            maximum_active: AtomicUsize::new(0),
            release: Semaphore::new(0),
            started: AtomicUsize::new(0),
        });
        let targets = addresses
            .into_iter()
            .enumerate()
            .map(|(endpoint_index, address)| HealthTarget {
                check: Arc::<BlockingCheck>::clone(&check),
                endpoint: RuntimeEndpoint::from(address),
                endpoint_index,
                healthy_threshold: 1,
                pool: Arc::clone(&pool),
                pool_name: "bounded".into(),
                probe_lock: Arc::new(Mutex::new(())),
                timeout: Duration::from_secs(5),
                unhealthy_threshold: 1,
            })
            .collect();
        let probes = tokio::spawn(run_targets(
            targets,
            Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
        ));

        timeout(Duration::from_secs(1), async {
            while check.started.load(Ordering::SeqCst) < MAX_CONCURRENT_PROBES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial probes must start");
        sleep(Duration::from_millis(25)).await;
        assert_eq!(check.started.load(Ordering::SeqCst), MAX_CONCURRENT_PROBES);
        assert_eq!(
            check.maximum_active.load(Ordering::SeqCst),
            MAX_CONCURRENT_PROBES
        );

        check.release.add_permits(33);
        probes.await.expect("bounded probes");
        assert_eq!(check.started.load(Ordering::SeqCst), 33);
    }
}
