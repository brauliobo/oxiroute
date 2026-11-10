#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn assert_fixed_response(mut stream: impl Read + Write) {
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

pub(super) fn control_http_response(address: SocketAddr, path: &str) -> String {
    let mut stream =
        TcpStream::connect_timeout(&address, IO_TIMEOUT).expect("connect control HTTP listener");
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .expect("control listener read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("write control HTTP request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read control HTTP response");
    response
}

pub(super) fn reserve_tcp_address() -> SocketAddr {
    ports_support::unused_address()
}

pub(super) fn verify_listener_descriptors(worker_pid: u32, listener_targets: &[PathBuf]) {
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

pub(super) fn listeners_only_config(tcp_address: SocketAddr, unix_path: PathBuf) -> ConfigDraft {
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
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners,
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: vec![HttpService {
            drain_deadline_ms: None,
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
        audit: None,
        log_filter: None,
    }
}

pub(super) fn udp_only_config(
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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
            passive_health: None,
            tls: None,
            http_versions: oxiroute_config::HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 10_000,
            lifetime_timeout_ms: Some(30_000),
            proxy_protocol: None,
            udp: Some(oxiroute_config::UdpPolicy::default()),
        }],
        audit: None,
        log_filter: None,
    }
}

pub(super) fn h3_only_config(listener_address: SocketAddr, private_key_path: &Path) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        audit: None,
        log_filter: None,
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
            drain_deadline_ms: None,
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

pub(super) fn replace_real_worker(
    master: &mut Master,
    config_path: &Path,
    revision: String,
    generation: u64,
    inject_runtime_failure: bool,
) {
    let token_byte = u8::try_from(0x50_u64 + generation).expect("test generation token");
    let token = [token_byte; 16];
    let identity = WorkerIdentity {
        instance: InstanceToken(token),
        generation: GenerationId(generation),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let mut command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(generation.to_string())
        .arg(encode_token(token))
        .arg(config_path)
        .arg(revision);
    if inject_runtime_failure {
        command = command.env(TEST_RUNTIME_FAILURE_ENV, "1");
    }
    let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
        .expect("production launcher implementation");
    master
        .replace(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new(&format!("oxiroute-stage-2-candidate-{generation}"))
                    .expect("candidate identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("start real replacement");
}

pub(super) async fn poll_master_until_async(
    harness: &mut Harness,
    mut predicate: impl FnMut(&Master) -> bool,
) -> Vec<MasterEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    while !predicate(&harness.master) {
        events.extend(harness.master.poll(Instant::now()).expect("master poll"));
        assert!(
            started.elapsed() < TEST_TIMEOUT,
            "master remained in {:?}; events: {events:?}",
            harness.master.state()
        );
        sleep(POLL_INTERVAL).await;
    }
    events.extend(
        harness
            .master
            .poll(Instant::now())
            .expect("final master poll"),
    );
    events
}

pub(super) fn assert_no_udp_datagram(socket: &StdUdpSocket) {
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("UDP probe timeout");
    let mut buffer = [0_u8; 128];
    assert!(
        socket.recv_from(&mut buffer).is_err(),
        "UDP listener admitted work while quiescing"
    );
}

pub(super) fn matching_descriptor_count(targets: &[PathBuf]) -> usize {
    fs::read_dir("/proc/self/fd")
        .expect("master descriptors")
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| targets.contains(target))
        .count()
}

pub(super) fn wait_process_gone(pid: u32) {
    let path = PathBuf::from(format!("/proc/{pid}"));
    eventually(&format!("process {pid} reaping"), || {
        if path.exists() {
            Err("still present")
        } else {
            Ok(())
        }
    });
}

pub(super) fn config_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/supervised-empty.kdl"
    ))
}

pub(super) fn supervisor_launcher() -> PathBuf {
    std::env::var_os(TEST_LAUNCHER_ENV).map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture")),
        PathBuf::from,
    )
}
