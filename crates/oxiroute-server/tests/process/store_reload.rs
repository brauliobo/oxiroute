#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/mod.rs"]
mod wire_support;

use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{
    CacheAuthorizationPolicy, CacheKeyComponent, CacheSetCookiePolicy, CacheStore, CacheVaryPolicy,
    ConfigDraft, DownstreamTimeoutPolicy, HttpCachePolicy, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy, Listener,
    Management, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use serde_json::Value;
use tokio::time::sleep;

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const LOCK_NAME: &str = ".oxiroute-cache.lock";
const WAIT: Duration = Duration::from_secs(15);

struct Origin {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
}

fn spawn_origin(body_bytes: usize) -> Origin {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("origin");
    let address = listener.local_addr().expect("origin address");
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let counted = Arc::clone(&counted);
            thread::spawn(move || serve(stream, &counted, body_bytes));
        }
    });
    Origin { address, requests }
}

fn serve(mut stream: TcpStream, counted: &AtomicUsize, body_bytes: usize) {
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) => received.push(byte[0]),
            _ => return,
        }
        if !received.ends_with(b"\r\n\r\n") {
            continue;
        }
        received.clear();
        counted.fetch_add(1, Ordering::SeqCst);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {body_bytes}\r\nCache-Control: public, max-age=300\r\n\
             Content-Type: text/plain\r\n\r\n"
        );
        if stream.write_all(head.as_bytes()).is_err()
            || stream.write_all(&vec![b'x'; body_bytes]).is_err()
        {
            return;
        }
    }
}

struct Instance {
    server: ServerProcess,
    management: SocketAddr,
    listener: SocketAddr,
    origin: Origin,
    root: PathBuf,
    config: ConfigDraft,
    sequence: u64,
    _directory: tempfile::TempDir,
}

fn disk_store(root: &Path, max_bytes: u64) -> CacheStore {
    CacheStore::Disk {
        name: "disk".into(),
        root_directory: root.to_owned(),
        max_bytes,
        max_files: 128,
        max_object_bytes: 4096,
        max_header_bytes: 2 * 1024,
        max_key_bytes: 1024,
        max_tag_bytes: 256,
        max_tags_per_object: 8,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    }
}

fn memory_store(max_entries: u64) -> CacheStore {
    CacheStore::Memory {
        name: "disk".into(),
        max_bytes: 1024 * 1024,
        max_entries,
        max_object_bytes: 4096,
        max_header_bytes: 2 * 1024,
        max_key_bytes: 1024,
        max_tag_bytes: 256,
        max_tags_per_object: 8,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    }
}

fn cached_config(
    management: SocketAddr,
    listener: SocketAddr,
    origin: SocketAddr,
    store: CacheStore,
) -> ConfigDraft {
    let mut config = ConfigDraft {
        management: Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        }),
        listeners: vec![Listener {
            name: "web".into(),
            bind: socket_bind(listener),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(64),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        cache_stores: vec![store],
        upstream_pools: vec![UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(origin)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: Some(1_000),
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy::default().into(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    };
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        unreachable!("the route is a proxy route");
    };
    policy.cache = Some(Box::new(disk_cache_policy()));
    config
}

fn disk_cache_policy() -> HttpCachePolicy {
    HttpCachePolicy {
        store: "disk".into(),
        methods: vec!["GET".into()],
        key_components: vec![
            CacheKeyComponent::Scheme,
            CacheKeyComponent::NormalizedHost,
            CacheKeyComponent::PathAndQuery,
        ],
        use_origin_cache_control: true,
        default_ttl_ms: 60_000,
        status_ttls: Vec::new(),
        grace_ms: 30_000,
        keep_ms: 300_000,
        revalidate: true,
        collapsed_forwarding: true,
        stale_on: Vec::new(),
        bypass_request: Vec::new(),
        no_store_request: Vec::new(),
        no_store_response: Vec::new(),
        set_cookie_policy: CacheSetCookiePolicy::default(),
        authorization_policy: CacheAuthorizationPolicy::default(),
        vary_policy: CacheVaryPolicy::default(),
        surrogate_tags: None,
        purge_authorization: None,
        compression: None,
    }
}

impl Instance {
    async fn start(body_bytes: usize, max_bytes: u64) -> Self {
        Self::start_with(body_bytes, |root| disk_store(root, max_bytes)).await
    }

    async fn start_memory(body_bytes: usize, max_entries: u64) -> Self {
        Self::start_with(body_bytes, |_| memory_store(max_entries)).await
    }

    async fn start_with(body_bytes: usize, store: impl FnOnce(&Path) -> CacheStore) -> Self {
        let origin = spawn_origin(body_bytes);
        let (management, listener) = (reserve_tcp_address(), reserve_tcp_address());
        let directory = tempfile::tempdir().expect("cache parent");
        let root = directory.path().join("cache");
        let config = cached_config(management, listener, origin.address, store(&root));
        let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(listener).await;
        Self {
            server,
            management,
            listener,
            origin,
            root,
            config,
            sequence: 0,
            _directory: directory,
        }
    }

    async fn status_of(&self, path: &str) -> u16 {
        http_request(self.listener, "GET", path, &[("Connection", "close")], &[])
            .await
            .status
    }

    async fn warm(&self, path: &str) {
        let before = self.origin.requests.load(Ordering::SeqCst);
        for _ in 0..2 {
            assert_eq!(self.status_of(path).await, 200);
        }
        assert_eq!(
            self.origin.requests.load(Ordering::SeqCst),
            before + 1,
            "{path} was not cached after one origin request"
        );
    }

    async fn active_revision(&self) -> Value {
        let authorization = format!("Bearer {TOKEN}");
        http_request(
            self.management,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json()["generation"]["activeRevision"]
            .clone()
    }

    async fn apply(&mut self, edit: impl FnOnce(&mut ConfigDraft)) {
        let before = self.active_revision().await;
        self.sequence += 1;
        self.config.max_connections = Some(1_000 + self.sequence);
        edit(&mut self.config);
        write_config(
            &self.server.config_path,
            &self.config.clone().validate().unwrap(),
        );
        wait_until("the new revision activates", || async {
            (self.active_revision().await != before).then_some(())
        })
        .await;
    }

    fn records(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.root)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".oxiroute-cache-") && name.ends_with(".record")
                            })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn wait_for_records(&self, count: usize) {
        wait_until("persisted cache records", || async {
            (self.records().len() == count).then_some(())
        })
        .await;
    }

    fn lock_owner(&self) -> Option<u32> {
        let inode = fs::metadata(self.root.join(LOCK_NAME))
            .ok()?
            .ino()
            .to_string();
        fs::read_to_string("/proc/locks")
            .ok()?
            .lines()
            .find_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                let identity = fields.get(5)?;
                (fields.get(1) == Some(&"FLOCK") && identity.rsplit(':').next() == Some(&inode))
                    .then(|| fields.get(4)?.parse().ok())
                    .flatten()
            })
    }
}

async fn wait_until<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_generation_swap_keeps_the_warm_disk_cache_and_the_store_lock() {
    let mut instance = Instance::start(256, 1024 * 1024).await;
    assert_eq!(instance.status_of("/warm").await, 200);
    instance.wait_for_records(1).await;
    assert_eq!(instance.lock_owner(), Some(instance.server.pid()));

    instance.apply(|_| {}).await;

    assert_eq!(instance.status_of("/warm").await, 200);
    assert_eq!(
        instance.origin.requests.load(Ordering::SeqCst),
        1,
        "the warm entry did not survive the generation swap"
    );
    assert_eq!(instance.lock_owner(), Some(instance.server.pid()));
    instance.server.shutdown();
}

#[tokio::test]
async fn a_generation_swap_applies_a_changed_disk_cache_limit() {
    let mut instance = Instance::start(3000, 1024 * 1024).await;
    for path in ["/a", "/b", "/c"] {
        assert_eq!(instance.status_of(path).await, 200);
    }
    instance.wait_for_records(3).await;

    let root = instance.root.clone();
    instance
        .apply(|config| {
            config.cache_stores = vec![disk_store(&root, 4096)];
        })
        .await;

    let kept = instance.records();
    assert_eq!(kept.len(), 1, "the new limit keeps one record: {kept:?}");
    let bytes = kept
        .iter()
        .map(|record| fs::metadata(record).expect("record").len())
        .sum::<u64>();
    assert!(bytes <= 4096, "{bytes} bytes exceed the new limit");
    assert_eq!(instance.status_of("/after").await, 200);
    instance.wait_for_records(1).await;
    instance.server.shutdown();
}

#[tokio::test]
async fn a_generation_that_cannot_claim_its_store_leaves_the_previous_owner_writing() {
    let mut instance = Instance::start(256, 1024 * 1024).await;
    assert_eq!(instance.status_of("/warm").await, 200);
    instance.wait_for_records(1).await;
    let before = instance.active_revision().await;

    let blocked = instance.root.with_file_name("blocked");
    fs::create_dir(&blocked).expect("blocked root");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("blocked mode");
    let foreign_lock = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(blocked.join(LOCK_NAME))
        .expect("foreign lock file");
    rustix::fs::flock(
        &foreign_lock,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("foreign owner locks the replacement store");
    instance.sequence += 1;
    instance.config.max_connections = Some(1_000 + instance.sequence);
    let CacheStore::Disk { root_directory, .. } = &mut instance.config.cache_stores[0] else {
        unreachable!("the store is a disk store");
    };
    *root_directory = blocked;
    write_config(
        &instance.server.config_path,
        &instance.config.clone().validate().unwrap(),
    );
    let stderr = instance
        .server
        .wait_for_stderr("owned by the active worker")
        .await;
    assert!(stderr.contains("disk cache store `disk`"), "{stderr}");

    assert_eq!(instance.active_revision().await, before);
    assert_eq!(instance.lock_owner(), Some(instance.server.pid()));
    assert_eq!(instance.status_of("/after-failure").await, 200);
    instance.wait_for_records(2).await;
    drop(foreign_lock);
    instance.server.shutdown();
}

#[tokio::test]
async fn a_generation_swap_keeps_the_warm_memory_cache() {
    let mut instance = Instance::start_memory(256, 100).await;
    instance.warm("/warm").await;

    instance.apply(|_| {}).await;

    assert_eq!(instance.status_of("/warm").await, 200);
    assert_eq!(
        instance.origin.requests.load(Ordering::SeqCst),
        1,
        "the warm entry did not survive the generation swap"
    );
    instance.server.shutdown();
}

#[tokio::test]
async fn a_generation_swap_resizes_the_memory_cache_in_place() {
    let mut instance = Instance::start_memory(256, 100).await;
    for path in ["/a", "/b", "/c"] {
        instance.warm(path).await;
    }

    instance
        .apply(|config| config.cache_stores = vec![memory_store(1)])
        .await;

    assert_eq!(instance.status_of("/c").await, 200);
    assert_eq!(
        instance.origin.requests.load(Ordering::SeqCst),
        3,
        "the most recent entry did not survive the resize"
    );
    assert_eq!(instance.status_of("/a").await, 200);
    assert_eq!(
        instance.origin.requests.load(Ordering::SeqCst),
        4,
        "an entry above the new bound survived the resize"
    );
    instance.server.shutdown();
}
