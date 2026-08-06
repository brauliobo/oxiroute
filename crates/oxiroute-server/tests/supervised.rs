#![cfg(target_os = "linux")]

#[path = "support/fixtures.rs"]
mod fixture_support;

use std::{
    fs,
    io::{self, Cursor, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket as StdUdpSocket},
    os::{fd::AsRawFd as _, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bytes::{Buf as _, Bytes};
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, DownstreamTimeoutPolicy,
    HttpPathSelector, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, L4Service,
    Listener, ListenerBind, Protocol, TlsProfile, TlsVersion, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_config_source::{ConfigFormat, render_config};
use oxiroute_server::{
    ListenerReservations,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};
use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterEvent, MasterState, ShutdownProgress,
    WorkerInput, WorkerRole,
};
use oxiroute_supervisor_process::{WorkerCommand, WorkerIdentity, WorkerSpawner};
use quinn::crypto::rustls::QuicClientConfig;
use rustix::fs::OFlags;
use tokio::time::{sleep, timeout};

const MARKER: &str = "--__oxiroute-worker-7f3c9d1e";
const TEST_RUNTIME_FAILURE_ENV: &str = "OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE";
const TEST_LISTENER_DUPLICATION_FAILURE_ENV: &str =
    "OXIROUTE_INTERNAL_TEST_LISTENER_DUPLICATION_FAILURE";
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN: [u8; 16] = [0x51; 16];

struct Harness {
    master: Master,
    worker_pid: u32,
    launcher_pid: u32,
    listener_targets: Vec<PathBuf>,
}

impl Harness {
    fn launch(revision: String, inject_runtime_failure: bool) -> Self {
        Self::launch_at(config_path(), revision, inject_runtime_failure)
    }

    fn launch_at(config_path: &Path, revision: String, inject_runtime_failure: bool) -> Self {
        Self::launch_at_with_failures(config_path, revision, inject_runtime_failure, false)
    }

    fn launch_at_with_failures(
        config_path: &Path,
        revision: String,
        inject_runtime_failure: bool,
        inject_listener_duplication_failure: bool,
    ) -> Self {
        let coordinator =
            CanonicalConfigCoordinator::new(config_path).expect("fixture coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("canonical fixture was rejected");
        };
        let reservations = ListenerReservations::prepare(&document.normalized_config, None)
            .expect("master listener reservations");
        let listener_targets = document
            .normalized_config
            .listeners
            .iter()
            .map(|listener| {
                let descriptor = reservations
                    .get(&listener.name)
                    .expect("configured listener reservation")
                    .duplicate_owned_fd()
                    .expect("listener target descriptor");
                fs::read_link(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
                    .expect("master listener descriptor target")
            })
            .collect();
        let mut factory = WorkerSpawner::new(
            env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture"),
            Duration::from_secs(5),
        )
        .expect("production launcher implementation");
        let identity = WorkerIdentity {
            instance: InstanceToken(TOKEN),
            generation: GenerationId(1),
            protocol: CONTROL_PROTOCOL_VERSION,
        };
        let mut command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
            .expect("real oxiroute worker")
            .arg(MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(TOKEN))
            .arg(config_path)
            .arg(revision);
        if inject_runtime_failure {
            command = command.env(TEST_RUNTIME_FAILURE_ENV, "1");
        }
        if inject_listener_duplication_failure {
            command = command.env(TEST_LISTENER_DUPLICATION_FAILURE_ENV, "1");
        }
        let listeners = reservations
            .into_stable_listeners(&document.normalized_config)
            .expect("stable master listeners");
        let master = Master::launch(
            MasterConfig::new(
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(6),
            )
            .expect("master deadlines"),
            listeners,
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2").expect("instance identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("launch supervised worker");
        let worker_pid = master
            .worker_id(WorkerRole::Active)
            .expect("initial worker pid");
        let launcher_pid = master
            .worker_process_group_id(WorkerRole::Active)
            .expect("launcher process group");
        Self {
            master,
            worker_pid,
            launcher_pid,
            listener_targets,
        }
    }

    fn poll_until(&mut self, expected: MasterState) {
        let started = Instant::now();
        while self.master.state() != expected {
            self.master.poll(Instant::now()).expect("master poll");
            assert!(
                started.elapsed() < TEST_TIMEOUT,
                "master remained in {:?}",
                self.master.state()
            );
            thread::sleep(POLL_INTERVAL);
        }
        self.master.poll(Instant::now()).expect("final master poll");
    }

    fn verify_reaped(&self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let worker = PathBuf::from(format!("/proc/{}/", self.worker_pid));
        let launcher = PathBuf::from(format!("/proc/{}/", self.launcher_pid));
        while (worker.exists() || launcher.exists()) && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        assert!(
            !worker.exists(),
            "worker remained after terminal master state"
        );
        assert!(
            !launcher.exists(),
            "launcher remained after terminal master state"
        );
    }
}

#[test]
fn internal_worker_marker_is_absent_from_cli_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("--help")
        .output()
        .expect("CLI help");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(MARKER));
}

#[test]
fn malformed_reserved_worker_marker_fails_closed_before_clap() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("--__oxiroute-worker-invalid")
        .output()
        .expect("malformed internal invocation");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid reserved worker marker"));
    assert!(!stderr.contains("Usage:"));
}

#[test]
fn supervised_worker_rejects_a_revision_mismatch_and_is_reaped() {
    let mut harness = Harness::launch("0".repeat(64), false);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

#[test]
fn supervised_worker_adopts_marker_activates_and_shuts_down_boundedly() {
    let mut harness = Harness::launch(canonical_revision(config_path()), false);
    harness.poll_until(MasterState::Running);
    verify_control_descriptor(harness.worker_pid);
    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
fn generation_runtime_death_closes_control_and_is_reaped_without_killing_the_worker_in_test() {
    let mut harness = Harness::launch(canonical_revision(config_path()), true);
    harness.poll_until(MasterState::Running);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

fn verify_control_descriptor(worker_pid: u32) {
    let descriptor_root = PathBuf::from(format!("/proc/{worker_pid}/fd"));
    assert_eq!(
        fs::read_link(descriptor_root.join("0")).expect("worker fd 0"),
        Path::new("/dev/null")
    );
    let sockets = fs::read_dir(&descriptor_root)
        .expect("worker descriptors")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            fs::read_link(entry.path())
                .ok()
                .and_then(|target| {
                    target
                        .to_string_lossy()
                        .strip_prefix("socket:[")
                        .and_then(|target| target.strip_suffix(']'))
                        .map(str::to_owned)
                })
                .map(|inode| (entry.file_name(), inode))
        })
        .collect::<Vec<_>>();
    assert!(!sockets.is_empty(), "worker had no control socket");
    let unix_socket_bytes =
        fs::read(format!("/proc/{worker_pid}/net/unix")).expect("worker Unix socket table");
    let unix_sockets = String::from_utf8_lossy(&unix_socket_bytes);
    let control = sockets
        .iter()
        .filter(|(_, inode)| {
            unix_sockets.lines().any(|line| {
                let columns = line.split_whitespace().collect::<Vec<_>>();
                columns.get(4) == Some(&"0005") && columns.get(6) == Some(&inode.as_str())
            })
        })
        .map(|(descriptor, _)| descriptor)
        .collect::<Vec<_>>();
    assert_eq!(control.len(), 1, "expected one SOCK_SEQPACKET control fd");
    let details =
        fs::read_to_string(PathBuf::from(format!("/proc/{worker_pid}/fdinfo")).join(control[0]))
            .expect("worker control flags");
    let flags = details
        .lines()
        .find_map(|line| line.strip_prefix("flags:\t"))
        .and_then(|value| u64::from_str_radix(value, 8).ok())
        .expect("parse worker control flags");
    assert_ne!(flags & u64::from(OFlags::CLOEXEC.bits()), 0);
}

fn encode_token(token: [u8; 16]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token {
        write!(encoded, "{byte:02x}").expect("write token");
    }
    encoded
}

fn canonical_revision(path: &Path) -> String {
    let coordinator = CanonicalConfigCoordinator::new(path).expect("fixture coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("canonical fixture was rejected");
    };
    document.candidate_revision.to_string()
}

#[test]
fn supervised_worker_serves_tcp_and_unix_http_from_transferred_descriptors() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary TCP bind")
        .local_addr()
        .expect("TCP address");
    let unix_path = directory.path().join("supervised.sock");
    let config = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render supervised config"),
    )
    .expect("write supervised config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);

    harness.poll_until(MasterState::Running);
    verify_listener_descriptors(harness.worker_pid, &harness.listener_targets);
    let tcp = TcpStream::connect(tcp_address).expect("connect transferred TCP listener");
    tcp.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("TCP read timeout");
    assert_fixed_response(tcp);
    let unix = UnixStream::connect(&unix_path).expect("connect transferred Unix listener");
    unix.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("Unix read timeout");
    assert_fixed_response(unix);

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
    assert!(
        unix_path.exists(),
        "master namespace owner was dropped early"
    );
    let marker = unix_path.with_file_name("supervised.sock.oxiroute.lock");
    assert!(marker.exists(), "master namespace lease was dropped early");
    drop(harness);
    assert!(
        !unix_path.exists(),
        "master did not clean up its Unix socket"
    );
    assert!(
        !marker.exists(),
        "master did not clean up its namespace marker"
    );
}

#[test]
fn supervised_worker_adopts_udp_and_reports_datagram_status() {
    let directory = tempfile::tempdir().expect("supervised UDP fixture directory");
    let upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP upstream");
    upstream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDP upstream timeout");
    let upstream_address = upstream.local_addr().expect("UDP upstream address");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("UDP listener probe")
        .local_addr()
        .expect("UDP listener address");
    let config = udp_only_config(listener_address, upstream_address);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render UDP config"),
    )
    .expect("write UDP config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);

    harness.poll_until(MasterState::Running);
    let status = harness
        .master
        .worker_status(WorkerRole::Active)
        .expect("active UDP worker status");
    assert_eq!(status.listeners.len(), 1);
    assert_eq!(status.listeners[0].name, "relay");
    assert_eq!(status.listeners[0].protocol, "udp");
    assert_eq!(
        status.listeners[0].state,
        oxiroute_supervisor_master::WorkerListenerState::Listening
    );

    let client = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP client");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDP client timeout");
    client
        .send_to(b"supervised-udp", listener_address)
        .expect("send UDP datagram");
    let mut buffer = [0_u8; 128];
    let (length, peer) = upstream
        .recv_from(&mut buffer)
        .expect("receive upstream datagram");
    assert_eq!(&buffer[..length], b"supervised-udp");
    upstream
        .send_to(b"supervised-response", peer)
        .expect("send upstream response");
    let (length, _) = client.recv_from(&mut buffer).expect("receive UDP response");
    assert_eq!(&buffer[..length], b"supervised-response");

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[tokio::test]
async fn supervised_worker_adopts_h3_and_serves_a_quic_request() {
    let directory = tempfile::tempdir().expect("supervised H3 fixture directory");
    let listener_address = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("H3 listener probe")
        .local_addr()
        .expect("H3 listener address");
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = h3_only_config(listener_address, key.path());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render H3 config"),
    )
    .expect("write H3 config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);

    harness.poll_until(MasterState::Running);
    verify_listener_descriptors(harness.worker_pid, &harness.listener_targets);
    let status = harness
        .master
        .worker_status(WorkerRole::Active)
        .expect("active H3 worker status");
    assert_eq!(status.listeners.len(), 1);
    assert_eq!(status.listeners[0].name, "h3");
    assert_eq!(status.listeners[0].protocol, "http3");
    assert_eq!(
        status.listeners[0].state,
        oxiroute_supervisor_master::WorkerListenerState::Listening
    );

    let endpoint = h3_client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, "proxy.example.test") {
                if let Ok(connection) = connecting.await {
                    break connection;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervised H3 connection timeout");
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = tokio::spawn(async move {
        let mut driver = driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri("https://proxy.example.test/")
        .body(())
        .expect("H3 request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    let body = body.copy_to_bytes(body.remaining());
    assert_eq!(body, Bytes::from_static(b"supervised-h3"));
    assert!(stream.recv_data().await.expect("H3 response end").is_none());

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
fn supervised_worker_replaces_a_same_manifest_generation() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary TCP bind")
        .local_addr()
        .expect("TCP address");
    let unix_path = directory.path().join("replacement.sock");
    let initial = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &initial).expect("render initial config"),
    )
    .expect("write initial config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let mut updated = initial;
    let HttpRouteAction::FixedResponse { body, .. } =
        &mut updated.http_services[0].routes[0].action
    else {
        panic!("fixture route is not a fixed response");
    };
    *body = "stage-3".into();
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &updated).expect("render replacement config"),
    )
    .expect("write replacement config");

    let revision = canonical_revision(&path);
    let identity = WorkerIdentity {
        instance: InstanceToken([0x52; 16]),
        generation: GenerationId(2),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(identity.generation.to_string())
        .arg(encode_token([0x52; 16]))
        .arg(&path)
        .arg(revision);
    let mut factory = WorkerSpawner::new(
        env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture"),
        Duration::from_secs(5),
    )
    .expect("production launcher implementation");
    harness
        .master
        .replace(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2-candidate")
                    .expect("candidate identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("start replacement");

    let started = Instant::now();
    let mut committed = false;
    while !committed || harness.master.state() != MasterState::Running {
        let events = harness.master.poll(Instant::now()).expect("master poll");
        committed |= events
            .iter()
            .any(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }));
        assert!(started.elapsed() < TEST_TIMEOUT, "replacement timed out");
        thread::sleep(POLL_INTERVAL);
    }
    let mut stream = TcpStream::connect(tcp_address).expect("replacement TCP connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("TCP read timeout");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write replacement request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read replacement response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("stage-3"), "{response}");

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
fn supervised_worker_reactivates_after_a_replacement_rejection() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary TCP bind")
        .local_addr()
        .expect("TCP address");
    let unix_path = directory.path().join("rollback.sock");
    let config = listeners_only_config(tcp_address, unix_path);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render rollback config"),
    )
    .expect("write rollback config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let identity = WorkerIdentity {
        instance: InstanceToken([0x53; 16]),
        generation: GenerationId(2),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(identity.generation.to_string())
        .arg(encode_token([0x53; 16]))
        .arg(&path)
        .arg("0".repeat(64));
    let mut factory = WorkerSpawner::new(
        env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture"),
        Duration::from_secs(5),
    )
    .expect("production launcher implementation");
    harness
        .master
        .replace(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2-rollback")
                    .expect("candidate identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("start rejected replacement");

    let started = Instant::now();
    let mut rolled_back = false;
    while !rolled_back || harness.master.state() != MasterState::Running {
        let events = harness.master.poll(Instant::now()).expect("master poll");
        rolled_back |= events
            .iter()
            .any(|event| matches!(event, MasterEvent::RollbackCompleted { .. }));
        assert!(started.elapsed() < TEST_TIMEOUT, "rollback timed out");
        thread::sleep(POLL_INTERVAL);
    }
    assert_fixed_response(TcpStream::connect(tcp_address).expect("rollback TCP connection"));

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
fn listener_duplication_failure_never_acknowledges_adoption_or_reaches_running() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary TCP bind")
        .local_addr()
        .expect("TCP address");
    let unix_path = directory.path().join("failure.sock");
    let config = listeners_only_config(tcp_address, unix_path);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render supervised config"),
    )
    .expect("write supervised config");
    let mut harness =
        Harness::launch_at_with_failures(&path, canonical_revision(&path), false, true);
    let started = Instant::now();
    while harness.master.state() != MasterState::Failed {
        let state = harness.master.state();
        assert!(
            !matches!(state, MasterState::ActivatingInitial | MasterState::Running),
            "failed listener startup advanced to {state:?}"
        );
        harness.master.poll(Instant::now()).expect("master poll");
        assert!(started.elapsed() < TEST_TIMEOUT);
        thread::sleep(POLL_INTERVAL);
    }
    harness.verify_reaped();
}

#[test]
fn supervised_listener_worker_crash_is_reaped_and_master_cleans_namespace() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("temporary TCP bind")
        .local_addr()
        .expect("TCP address");
    let unix_path = directory.path().join("crash.sock");
    let config = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(ConfigFormat::Kdl, &config).expect("render supervised config"),
    )
    .expect("write supervised config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), true);

    harness.poll_until(MasterState::Running);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
    drop(harness);
    assert!(
        !unix_path.exists(),
        "failed master did not clean up Unix socket"
    );
}

fn assert_fixed_response(mut stream: impl Read + Write) {
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write HTTP request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("stage-2"), "{response}");
}

fn verify_listener_descriptors(worker_pid: u32, listener_targets: &[PathBuf]) {
    let descriptor_root = PathBuf::from(format!("/proc/{worker_pid}/fd"));
    let descriptors = fs::read_dir(&descriptor_root)
        .expect("worker descriptors")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            fs::read_link(entry.path())
                .ok()
                .map(|target| (entry, target))
        })
        .collect::<Vec<_>>();
    for target in listener_targets {
        let matching = descriptors
            .iter()
            .filter(|(_, descriptor_target)| descriptor_target == target)
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            2,
            "worker did not retain exactly one adopted and one runtime descriptor for {}",
            target.display()
        );
        for (entry, _) in matching {
            let details = fs::read_to_string(
                PathBuf::from(format!("/proc/{worker_pid}/fdinfo")).join(entry.file_name()),
            )
            .expect("worker listener flags");
            let flags = details
                .lines()
                .find_map(|line| line.strip_prefix("flags:\t"))
                .and_then(|value| u64::from_str_radix(value, 8).ok())
                .expect("parse worker listener flags");
            assert_ne!(flags & u64::from(OFlags::CLOEXEC.bits()), 0);
        }
    }
}

fn listeners_only_config(tcp_address: SocketAddr, unix_path: PathBuf) -> Config {
    let listeners = [
        (
            "tcp",
            ListenerBind::Socket {
                address: tcp_address,
            },
        ),
        (
            "unix",
            ListenerBind::Unix {
                path: unix_path,
                mode: Some(0o600),
            },
        ),
    ]
    .into_iter()
    .map(|(name, bind)| Listener {
        name: name.into(),
        bind,
        protocol: Protocol::Http,
        service: Some("fixed".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    })
    .collect();
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners,
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: vec![HttpService {
            name: "fixed".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "stage-2".into(),
                    headers: Vec::new(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 1_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn udp_only_config(listener_address: SocketAddr, upstream_address: SocketAddr) -> Config {
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: vec![Listener {
            name: "relay".into(),
            bind: ListenerBind::Udp {
                address: listener_address,
            },
            protocol: Protocol::Udp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        cache_stores: Vec::new(),
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![UpstreamEndpoint::Socket {
                address: upstream_address,
            }],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: None,
            http_versions: oxiroute_config::HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
        }],
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: vec![L4Service {
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 10_000,
            lifetime_timeout_ms: Some(30_000),
            proxy_protocol: None,
            udp: Some(oxiroute_config::UdpPolicy::default()),
        }],
    }
}

fn h3_only_config(listener_address: SocketAddr, private_key_path: &Path) -> Config {
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: vec![Certificate {
            name: "downstream".into(),
            dns_names: vec!["proxy.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
                private_key_path: private_key_path.to_owned(),
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "downstream".into(),
            certificates: vec!["downstream".into()],
            default_certificate: "downstream".into(),
            min_version: TlsVersion::Tls13,
            alpn: vec![AlpnProtocol::H3],
            policy: oxiroute_config::TlsPolicy::default(),
        }],
        listeners: vec![Listener {
            name: "h3".into(),
            bind: ListenerBind::Udp {
                address: listener_address,
            },
            protocol: Protocol::Http3,
            service: Some("h3".into()),
            tls_profile: Some("downstream".into()),
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: vec![HttpService {
            name: "h3".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy {
                    max_request_body_bytes: Some(64 * 1024),
                    request_buffering: true,
                    ..HttpRoutePolicy::default()
                },
                action: HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "supervised-h3".into(),
                    headers: Vec::new(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 5_000,
            max_request_body_bytes: Some(64 * 1024),
            gzip: None,
            access_log: None,
        }],
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn h3_client_endpoint() -> io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(fixture_support::fixture("ca-a.pem"))?;
    for certificate in rustls_pemfile::certs(&mut Cursor::new(ca)) {
        roots
            .add(certificate.map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    }
    let mut crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).map_err(io::Error::other)?,
    ));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

fn config_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/supervised-empty.kdl"
    ))
}
