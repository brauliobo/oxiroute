#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
pub(super) fn internal_worker_marker_is_absent_from_cli_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .arg("--help")
        .output()
        .expect("CLI help");
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(MARKER));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(MARKER));
}

#[test]
pub(super) fn malformed_reserved_worker_marker_fails_closed_before_clap() {
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
pub(super) fn supervised_worker_rejects_a_revision_mismatch_and_is_reaped() {
    let mut harness = Harness::launch("0".repeat(64), false);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

#[test]
pub(super) fn supervised_worker_adopts_marker_activates_and_shuts_down_boundedly() {
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
pub(super) fn generation_runtime_death_closes_control_and_is_reaped_without_killing_the_worker_in_test()
 {
    let mut harness = Harness::launch(canonical_revision(config_path()), true);
    harness.poll_until(MasterState::Failed);
    harness.verify_reaped();
}

pub(super) fn verify_control_descriptor(worker_pid: u32) {
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

pub(super) fn encode_token(token: [u8; 16]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token {
        write!(encoded, "{byte:02x}").expect("write token");
    }
    encoded
}

pub(super) fn canonical_revision(path: &Path) -> String {
    let coordinator = CanonicalConfigCoordinator::new(path).expect("fixture coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("canonical fixture was rejected");
    };
    document.effective_revision.to_string()
}

#[test]
pub(super) fn supervised_worker_serves_tcp_and_unix_http_from_transferred_descriptors() {
    let directory = tempfile::tempdir().expect("supervised fixture directory");
    let tcp_address = ports_support::unused_address();
    let unix_path = directory.path().join("supervised.sock");
    let config = listeners_only_config(tcp_address, unix_path.clone());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid supervised config"),
        ),
    )
    .expect("write supervised config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);

    harness.poll_until(MasterState::Running);
    verify_listener_descriptors(harness.worker_pid, &harness.listener_targets);
    let tcp = TcpStream::connect(tcp_address).expect("connect transferred TCP listener");
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .expect("TCP read timeout");
    assert_fixed_response(tcp);
    let unix = UnixStream::connect(&unix_path).expect("connect transferred Unix listener");
    unix.set_read_timeout(Some(IO_TIMEOUT))
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
pub(super) fn supervised_worker_adopts_udp_and_reports_datagram_status() {
    let directory = tempfile::tempdir().expect("supervised UDP fixture directory");
    let upstream = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("UDP upstream");
    upstream
        .set_read_timeout(Some(IO_TIMEOUT))
        .expect("UDP upstream timeout");
    let upstream_address = upstream.local_addr().expect("UDP upstream address");
    let listener_address = ports_support::unused_address();
    let config = udp_only_config(listener_address, upstream_address);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid UDP config"),
        ),
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
        .set_read_timeout(Some(IO_TIMEOUT))
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

#[test]
#[allow(clippy::too_many_lines)]
pub(super) fn supervised_control_listeners_report_real_lifecycle_and_connection_counters_once() {
    let directory = tempfile::tempdir().expect("control listener fixture directory");
    let management = reserve_tcp_address();
    let stats_address = reserve_tcp_address();
    let stats_page = reserve_tcp_address();
    let config = ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: Some(Management {
            bind: management,
            ui_dir: None,
        }),
        registry: None,
        stats: Some(Stats {
            binds: vec![stats_address],
            admin_token_file: None,
            pages: vec![StatsPage {
                bind: stats_page,
                uri_prefix: "/stats".into(),
                refresh_ms: 1_000,
                admin: StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
        }),
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    };
    let path = directory.path().join("control-listeners.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.validate().expect("valid control listener config"),
        ),
    )
    .expect("write control listener config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    let management_stream =
        TcpStream::connect_timeout(&management, IO_TIMEOUT).expect("connect management listener");
    drop(management_stream);
    let metrics = control_http_response(stats_address, "/metrics");
    assert!(metrics.starts_with("HTTP/1.1 200"), "{metrics}");
    assert!(!metrics.contains("@management"));
    assert!(!metrics.contains("@stats-0"));
    assert!(metrics.contains("@stats-page-0"));
    let page = control_http_response(stats_page, "/stats");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    // An idle worker looks for a change once a second.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let status = loop {
        harness.master.poll(Instant::now()).expect("status poll");
        let status = harness
            .master
            .worker_status(WorkerRole::Active)
            .expect("active worker status");
        if status
            .listeners
            .iter()
            .all(|listener| listener.accepted_connections > 0)
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the counters never reached the master"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.listeners.len(), 3);
    for name in ["@management", "@stats-0", "@stats-page-0"] {
        let matching = status
            .listeners
            .iter()
            .filter(|listener| listener.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "{name} was not reported exactly once");
        assert_eq!(
            matching[0].state,
            oxiroute_supervisor_master::WorkerListenerState::Listening
        );
        assert!(matching[0].accepted_connections > 0);
        assert_eq!(matching[0].active_connections, 0);
    }

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[test]
pub(super) fn supervised_control_listener_callback_failure_prevents_readiness() {
    let directory = tempfile::tempdir().expect("control listener failure fixture directory");
    let config = ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: Some(Management {
            bind: reserve_tcp_address(),
            ui_dir: None,
        }),
        registry: None,
        stats: Some(Stats {
            binds: vec![reserve_tcp_address()],
            admin_token_file: None,
            pages: vec![StatsPage {
                bind: reserve_tcp_address(),
                uri_prefix: "/stats".into(),
                refresh_ms: 1_000,
                admin: StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            }],
        }),
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    };
    let path = directory.path().join("control-listener-failure.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.validate().expect("valid control listener config"),
        ),
    )
    .expect("write control listener config");

    let mut harness =
        Harness::launch_at_with_failures(&path, canonical_revision(&path), false, true);
    harness.poll_until(MasterState::Failed);
    assert!(harness.master.worker_status(WorkerRole::Active).is_none());
    harness.verify_reaped();
}
