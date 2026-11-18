use super::reload::{finish_reload_harness, read_until, reload_config};
#[allow(clippy::wildcard_imports)]
use super::*;

use std::{
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    sync::atomic::{AtomicUsize, Ordering},
};

use oxiroute_config::{
    CacheAuthorizationPolicy, CacheKeyComponent, CacheSetCookiePolicy, CacheStore, CacheVaryPolicy,
    HttpCachePolicy,
};

const LOCK_NAME: &str = ".oxiroute-cache.lock";

struct CountingOrigin {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    release_gated: std::sync::mpsc::Sender<()>,
}

fn spawn_counting_origin(body_bytes: usize) -> CountingOrigin {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("origin");
    let address = listener.local_addr().expect("origin address");
    let requests = Arc::new(AtomicUsize::new(0));
    let (release_gated, gate) = std::sync::mpsc::channel();
    let gate = Arc::new(std::sync::Mutex::new(gate));
    let counted = Arc::clone(&requests);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let counted = Arc::clone(&counted);
            let gate = Arc::clone(&gate);
            thread::spawn(move || serve_counted(stream, &counted, &gate, body_bytes));
        }
    });
    CountingOrigin {
        address,
        requests,
        release_gated,
    }
}

fn serve_counted(
    mut stream: TcpStream,
    counted: &AtomicUsize,
    gate: &std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    body_bytes: usize,
) {
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
        let gated = received.starts_with(b"GET /gated ");
        received.clear();
        counted.fetch_add(1, Ordering::SeqCst);
        if gated {
            let _ = gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv();
        }
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

struct CachedShape {
    address: SocketAddr,
    origin: SocketAddr,
    root: PathBuf,
    max_bytes: u64,
    max_object_bytes: u64,
    io_timeout_ms: u64,
}

fn cached_config(shape: &CachedShape) -> ConfigDraft {
    let mut config = reload_config(&ReloadShape {
        http: &[("web", shape.address)],
        relay: None,
        origin: Some(shape.origin),
        body: "unused",
    });
    config.http_services[0].upstream_io_timeout_ms = shape.io_timeout_ms;
    config.cache_stores = vec![CacheStore::Disk {
        name: "disk".into(),
        root_directory: shape.root.clone(),
        max_bytes: shape.max_bytes,
        max_files: 128,
        max_object_bytes: shape.max_object_bytes,
        max_header_bytes: 2 * 1024,
        max_key_bytes: 1024,
        max_tag_bytes: 256,
        max_tags_per_object: 8,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    }];
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        panic!("cached route is a proxy route");
    };
    policy.cache = Some(Box::new(HttpCachePolicy {
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
    }));
    config
}

fn write_cached_config(path: &Path, shape: &CachedShape) {
    fs::write(
        path,
        render_config(
            ConfigFormat::Kdl,
            &cached_config(shape)
                .validate()
                .expect("valid cached config"),
        ),
    )
    .expect("write cached config");
}

fn launch_cached_harness(path: &Path, shape: &CachedShape) -> Harness {
    write_cached_config(path, shape);
    let mut harness = Harness::launch_at(path, canonical_revision(path), false);
    harness.poll_until(MasterState::Running);
    harness
}

fn get_status_line(stream: &mut TcpStream, path: &str) -> io::Result<String> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    while !received.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        received.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&received).into_owned();
    let length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .expect("content length");
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body)?;
    Ok(head.lines().next().unwrap_or_default().to_owned())
}

fn fetch(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).expect("connect");
    get_status_line(&mut stream, path).expect("response")
}

fn record_files(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
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

fn wait_for_records(root: &Path, count: usize) {
    eventually("persisted cache records", || {
        let found = record_files(root).len();
        if found == count {
            Ok(())
        } else {
            Err(format!("{found} records, expected {count}"))
        }
    });
}

pub(super) fn lock_owner(lock: &Path) -> Option<u32> {
    let inode = fs::metadata(lock).ok()?.ino().to_string();
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

pub(super) fn wait_for_lock_owner(lock: &Path, owner: u32) {
    eventually("store lock owner", || match lock_owner(lock) {
        Some(found) if found == owner => Ok(()),
        found => Err(format!("{found:?}, expected {owner}")),
    });
}

pub(super) fn worker(harness: &Harness, role: WorkerRole) -> u32 {
    harness.master.worker_id(role).expect("worker of role")
}

#[test]
fn reload_keeps_the_warm_disk_cache_and_moves_the_store_to_the_new_worker() {
    let directory = tempfile::tempdir().expect("cache fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let root = directory.path().join("cache");
    let origin = spawn_counting_origin(256);
    let address = reserve_tcp_address();
    let shape = |io_timeout_ms| CachedShape {
        address,
        origin: origin.address,
        root: root.clone(),
        max_bytes: 1024 * 1024,
        max_object_bytes: 4096,
        io_timeout_ms,
    };
    let mut harness = launch_cached_harness(&path, &shape(120_000));
    let first_worker = worker(&harness, WorkerRole::Active);
    assert_eq!(fetch(address, "/warm"), "HTTP/1.1 200 OK");
    wait_for_records(&root, 1);
    assert_eq!(origin.requests.load(Ordering::SeqCst), 1);
    wait_for_lock_owner(&root.join(LOCK_NAME), first_worker);

    write_cached_config(&path, &shape(130_000));
    harness.start_reload(&path).expect("start the reload");
    harness.wait_committed();

    let second_worker = worker(&harness, WorkerRole::Active);
    assert_ne!(second_worker, first_worker);
    wait_for_lock_owner(&root.join(LOCK_NAME), second_worker);
    assert_eq!(fetch(address, "/warm"), "HTTP/1.1 200 OK");
    assert_eq!(
        origin.requests.load(Ordering::SeqCst),
        1,
        "the warm entry did not survive the reload"
    );

    harness.wait_retired_worker_exit();
    wait_for_lock_owner(&root.join(LOCK_NAME), second_worker);
    finish_reload_harness(harness);
}

#[test]
fn reload_applies_a_changed_disk_cache_limit() {
    let directory = tempfile::tempdir().expect("cache fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let root = directory.path().join("cache");
    let origin = spawn_counting_origin(3000);
    let address = reserve_tcp_address();
    let shape = |max_bytes| CachedShape {
        address,
        origin: origin.address,
        root: root.clone(),
        max_bytes,
        max_object_bytes: 4096,
        io_timeout_ms: 120_000,
    };
    let mut harness = launch_cached_harness(&path, &shape(1024 * 1024));
    for entry in ["/a", "/b", "/c"] {
        assert_eq!(fetch(address, entry), "HTTP/1.1 200 OK");
    }
    wait_for_records(&root, 3);

    write_cached_config(&path, &shape(4096));
    harness.start_reload(&path).expect("start the reload");
    harness.wait_committed();

    let kept = record_files(&root);
    assert_eq!(kept.len(), 1, "the new limit keeps one record: {kept:?}");
    let bytes = kept
        .iter()
        .map(|record| fs::metadata(record).expect("record").len())
        .sum::<u64>();
    assert!(bytes <= 4096, "{bytes} bytes exceed the new limit");
    harness.wait_retired_worker_exit();
    finish_reload_harness(harness);
}

#[test]
fn overlapping_workers_never_both_write_the_store() {
    let directory = tempfile::tempdir().expect("cache fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let root = directory.path().join("cache");
    let origin = spawn_counting_origin(256);
    let address = reserve_tcp_address();
    let shape = |io_timeout_ms| CachedShape {
        address,
        origin: origin.address,
        root: root.clone(),
        max_bytes: 1024 * 1024,
        max_object_bytes: 4096,
        io_timeout_ms,
    };
    let mut harness = launch_cached_harness(&path, &shape(120_000));
    assert_eq!(fetch(address, "/warm"), "HTTP/1.1 200 OK");
    wait_for_records(&root, 1);
    let mut gated = TcpStream::connect_timeout(&address, IO_TIMEOUT).expect("connect");
    gated.set_read_timeout(Some(IO_TIMEOUT)).expect("timeout");
    gated
        .write_all(b"GET /gated HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("gated request");
    eventually("the gated request reaching the origin", || {
        let arrived = origin.requests.load(Ordering::SeqCst);
        if arrived == 2 {
            Ok(())
        } else {
            Err(format!("{arrived} requests"))
        }
    });

    write_cached_config(&path, &shape(130_000));
    harness.start_reload(&path).expect("start the reload");
    harness.wait_committed();
    let (retiring, active) = (
        worker(&harness, WorkerRole::Retired),
        worker(&harness, WorkerRole::Active),
    );
    assert_ne!(retiring, active);
    wait_for_lock_owner(&root.join(LOCK_NAME), active);

    origin
        .release_gated
        .send(())
        .expect("release the gated response");
    read_until(&mut gated, b"\r\n\r\n");
    gated
        .read_exact(&mut [0_u8; 256])
        .expect("gated response body");
    assert_eq!(fetch(address, "/by-active"), "HTTP/1.1 200 OK");
    wait_for_records(&root, 2);
    assert_eq!(lock_owner(&root.join(LOCK_NAME)), Some(active));

    drop(gated);
    harness.wait_retired_worker_exit();
    assert_eq!(
        record_files(&root).len(),
        2,
        "the retiring worker wrote a record while the active worker owned the store"
    );
    finish_reload_harness(harness);
}

#[test]
fn a_replacement_that_cannot_claim_its_store_rolls_back_to_the_previous_owner() {
    let directory = tempfile::tempdir().expect("cache fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let root = directory.path().join("cache");
    let blocked = directory.path().join("blocked");
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
    let origin = spawn_counting_origin(256);
    let address = reserve_tcp_address();
    let shape = |root: &Path| CachedShape {
        address,
        origin: origin.address,
        root: root.to_owned(),
        max_bytes: 1024 * 1024,
        max_object_bytes: 4096,
        io_timeout_ms: 120_000,
    };
    let mut harness = launch_cached_harness(&path, &shape(&root));
    let owner = worker(&harness, WorkerRole::Active);
    assert_eq!(fetch(address, "/warm"), "HTTP/1.1 200 OK");
    wait_for_records(&root, 1);

    write_cached_config(&path, &shape(&blocked));
    harness.start_reload(&path).expect("start the reload");
    harness.poll_events_until(|event| matches!(event, MasterEvent::RollbackCompleted { .. }));
    harness.poll_until(MasterState::Running);

    assert_eq!(worker(&harness, WorkerRole::Active), owner);
    assert_eq!(lock_owner(&root.join(LOCK_NAME)), Some(owner));
    assert_eq!(fetch(address, "/after-rollback"), "HTTP/1.1 200 OK");
    wait_for_records(&root, 2);
    assert!(record_files(&blocked).is_empty());
    drop(foreign_lock);
    finish_reload_harness(harness);
}
