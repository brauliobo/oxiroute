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
        down_interval: Duration::from_secs(1),
        endpoint,
        endpoint_index: 0,
        fast_interval: Duration::from_secs(1),
        healthy_threshold: 1,
        interval: Duration::from_secs(1),
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

#[test]
fn health_intervals_follow_checking_down_and_healthy_state() {
    let endpoint = RuntimeEndpoint::from(std::net::SocketAddr::from(([127, 0, 0, 1], 3_000)));
    let pool = Arc::new(
        RoundRobinPool::new_named(
            "intervals".into(),
            [endpoint.clone()],
            oxiroute_config::UpstreamAlgorithm::First,
            true,
        )
        .expect("health pool"),
    );
    let target = HealthTarget {
        check: Arc::new(TcpHealthCheck::default()),
        down_interval: Duration::from_secs(5),
        endpoint,
        endpoint_index: 0,
        fast_interval: Duration::from_secs(1),
        healthy_threshold: 1,
        interval: Duration::from_secs(2),
        pool: Arc::clone(&pool),
        pool_name: "intervals".into(),
        probe_lock: Arc::new(Mutex::new(())),
        timeout: Duration::from_millis(100),
        unhealthy_threshold: 1,
    };

    assert_eq!(target.next_interval(), Duration::from_secs(1));
    pool.record_health(0, false, Some(HealthFailure::ConnectFailed), Some(1), 1, 1);
    assert_eq!(target.next_interval(), Duration::from_secs(5));
    pool.record_health(0, true, None, Some(2), 1, 1);
    assert_eq!(target.next_interval(), Duration::from_secs(2));
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
            down_interval: Duration::from_secs(1),
            endpoint: RuntimeEndpoint::from(address),
            endpoint_index,
            fast_interval: Duration::from_secs(1),
            healthy_threshold: 1,
            interval: Duration::from_secs(1),
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
