#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn reload_config(shape: &ReloadShape<'_>) -> ConfigDraft {
    let mut config = listeners_only_config(reserve_tcp_address(), PathBuf::from("/unused"));
    if let Some(origin) = shape.origin {
        config.upstream_pools = vec![UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![UpstreamEndpoint::Socket { address: origin }],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }];
        config.http_services[0].upstream_io_timeout_ms = 120_000;
        config.http_services[0].routes[0].action = HttpRouteAction::Proxy {
            upstream_pool: "origin".into(),
            policy: HttpProxyPolicy::default().into(),
        };
    }
    config.listeners = shape
        .http
        .iter()
        .map(|(name, address)| Listener {
            name: (*name).into(),
            bind: ListenerBind::Socket { address: *address },
            protocol: Protocol::Http,
            service: Some("fixed".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        })
        .collect();
    if let HttpRouteAction::FixedResponse { body, .. } =
        &mut config.http_services[0].routes[0].action
    {
        *body = shape.body.into();
    }
    if let Some((name, address, upstream)) = shape.relay {
        config.listeners.push(Listener {
            name: name.into(),
            bind: ListenerBind::Socket { address },
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        });
        config
            .upstream_pools
            .extend(udp_only_config(address, upstream).upstream_pools);
        config.l4_services = vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 120_000,
            lifetime_timeout_ms: Some(600_000),
            proxy_protocol: None,
            udp: None,
        }];
    }
    config
}

pub(super) fn write_reload_config(path: &Path, shape: &ReloadShape<'_>) {
    fs::write(
        path,
        render_config(
            ConfigFormat::Kdl,
            &reload_config(shape)
                .validate()
                .expect("valid reload config"),
        ),
    )
    .expect("write reload config");
}

pub(super) fn launch_reload_harness(
    path: &Path,
    shape: &ReloadShape<'_>,
    drain: Duration,
) -> Harness {
    write_reload_config(path, shape);
    let mut harness =
        Harness::launch_with_drain(path, canonical_revision(path), false, None, drain);
    harness.poll_until(MasterState::Running);
    harness
}

pub(super) fn finish_reload_harness(mut harness: Harness) {
    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
}

/// Sends one keep-alive request and returns the response body.
pub(super) fn keep_alive_body(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    while !received.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        received.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&received).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .expect("fixed response length");
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

pub(super) fn fresh_body(address: SocketAddr) -> String {
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)
        .unwrap_or_else(|error| panic!("connect {address}: {error}"));
    keep_alive_body(&mut stream).unwrap_or_else(|error| panic!("request {address}: {error}"))
}

pub(super) fn assert_refuses_connections(address: SocketAddr) {
    let started = Instant::now();
    loop {
        match TcpStream::connect_timeout(&address, Duration::from_secs(1)) {
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => return,
            other => {
                assert!(
                    started.elapsed() < TEST_TIMEOUT,
                    "{address} still accepts connections: {other:?}"
                );
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

pub(super) fn assert_can_bind_again(address: SocketAddr) {
    eventually(&format!("{address} release"), || {
        std::net::TcpListener::bind(address)
    });
}

pub(super) fn spawn_echo_upstream() -> SocketAddr {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo upstream");
    let address = listener.local_addr().expect("echo upstream address");
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut stream = stream;
                let mut buffer = [0_u8; 256];
                while let Ok(count) = stream.read(&mut buffer) {
                    if count == 0 || stream.write_all(&buffer[..count]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    address
}

pub(super) fn echo(stream: &mut TcpStream, message: &[u8]) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(message)?;
    let mut echoed = vec![0_u8; message.len()];
    stream.read_exact(&mut echoed)?;
    assert_eq!(echoed, message);
    Ok(())
}

/// Origin that starts a 12 byte response and finishes it only when the test releases it, so a
/// response is in flight for as long as the test needs.
pub(super) fn spawn_streaming_origin() -> (SocketAddr, std::sync::mpsc::Sender<()>) {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("origin");
    let address = listener.local_addr().expect("origin address");
    let (release, released) = std::sync::mpsc::channel::<()>();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut received = Vec::new();
            let mut byte = [0_u8; 1];
            while !received.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => received.push(byte[0]),
                    _ => return,
                }
            }
            let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n6\r\nfirst-\r\n";
            if stream.write_all(head).is_err() || stream.flush().is_err() {
                return;
            }
            if released.recv().is_err() {
                return;
            }
            let _ = stream.write_all(b"6\r\nsecond\r\n0\r\n\r\n");
        }
    });
    (address, release)
}

/// Opens a request whose response has begun but is not finished.
pub(super) fn start_streaming_request(address: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)
        .unwrap_or_else(|error| panic!("connect {address}: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("stream read timeout");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write streaming request");
    read_until(&mut stream, b"first-");
    stream
}

/// Finishes the response of [`start_streaming_request`] after the origin released it.
pub(super) fn finish_streaming_request(stream: &mut TcpStream) {
    read_until(stream, b"0\r\n\r\n");
}

pub(super) fn read_until(stream: &mut TcpStream, marker: &[u8]) {
    let mut received = Vec::new();
    let mut byte = [0_u8; 1];
    while !received
        .windows(marker.len())
        .any(|window| window == marker)
    {
        let count = stream
            .read(&mut byte)
            .unwrap_or_else(|error| panic!("waiting for {marker:?}: {error}; got {received:?}"));
        assert_eq!(count, 1, "the response ended early: {received:?}");
        received.push(byte[0]);
    }
}

#[test]
pub(super) fn reload_adds_a_listener_and_keeps_the_existing_one_and_its_requests() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let (first, added) = (reserve_tcp_address(), reserve_tcp_address());
    let (origin, release) = spawn_streaming_origin();
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("first", first)],
            relay: None,
            origin: Some(origin),
            body: "v1",
        },
        Duration::from_secs(10),
    );
    let mut in_flight = start_streaming_request(first);

    write_reload_config(
        &path,
        &ReloadShape {
            http: &[("first", first), ("added", added)],
            relay: None,
            origin: None,
            body: "v2",
        },
    );
    let changes = harness.start_reload(&path).expect("bind the new listener");
    assert_eq!(changes.added, [format!("added (tcp {added})")]);
    assert_eq!(changes.kept, [format!("first (tcp {first})")]);
    assert!(changes.removed.is_empty());
    harness.wait_committed();

    // The new listener serves the new revision and so does the kept address, which was never
    // closed; the response in flight before the reload is finished by the old worker.
    assert_eq!(fresh_body(added), "v2");
    assert_eq!(fresh_body(first), "v2");
    release.send(()).expect("release the origin");
    finish_streaming_request(&mut in_flight);

    drop(in_flight);
    harness.wait_retired_worker_exit();
    assert_eq!(fresh_body(added), "v2");
    finish_reload_harness(harness);
}

#[test]
pub(super) fn reload_removes_a_listener_drains_its_requests_and_releases_the_address() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let (kept, removed) = (reserve_tcp_address(), reserve_tcp_address());
    let (origin, release) = spawn_streaming_origin();
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("kept", kept), ("removed", removed)],
            relay: None,
            origin: Some(origin),
            body: "v1",
        },
        Duration::from_secs(10),
    );
    let mut in_flight = start_streaming_request(removed);

    write_reload_config(
        &path,
        &ReloadShape {
            http: &[("kept", kept)],
            relay: None,
            origin: None,
            body: "v2",
        },
    );
    let changes = harness
        .start_reload(&path)
        .expect("reload without a listener");
    assert_eq!(changes.removed, [format!("removed (tcp {removed})")]);
    assert!(changes.added.is_empty());
    harness.wait_committed();

    assert_eq!(fresh_body(kept), "v2");
    assert_refuses_connections(removed);
    // The listening socket stops at the commit, so the address is free again while the request
    // accepted before the reload is still being served.
    assert_can_bind_again(removed);
    release.send(()).expect("release the origin");
    finish_streaming_request(&mut in_flight);

    drop(in_flight);
    harness.wait_retired_worker_exit();
    assert_can_bind_again(removed);
    assert_eq!(fresh_body(kept), "v2");
    finish_reload_harness(harness);
}

#[test]
pub(super) fn reload_moves_a_listener_to_another_address_without_dropping_its_requests() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let (before, after) = (reserve_tcp_address(), reserve_tcp_address());
    let (origin, release) = spawn_streaming_origin();
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("web", before)],
            relay: None,
            origin: Some(origin),
            body: "v1",
        },
        Duration::from_secs(10),
    );
    let mut in_flight = start_streaming_request(before);

    write_reload_config(
        &path,
        &ReloadShape {
            http: &[("web", after)],
            relay: None,
            origin: None,
            body: "v2",
        },
    );
    let changes = harness.start_reload(&path).expect("move the listener");
    assert_eq!(changes.added, [format!("web (tcp {after})")]);
    assert_eq!(changes.removed, [format!("web (tcp {before})")]);
    assert!(changes.kept.is_empty());
    harness.wait_committed();

    assert_eq!(fresh_body(after), "v2");
    assert_refuses_connections(before);
    release.send(()).expect("release the origin");
    finish_streaming_request(&mut in_flight);

    drop(in_flight);
    harness.wait_retired_worker_exit();
    assert_can_bind_again(before);
    finish_reload_harness(harness);
}

#[test]
pub(super) fn reload_that_cannot_bind_a_new_listener_changes_nothing_and_can_be_retried() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let (running, blocked_by_another_process) = (reserve_tcp_address(), reserve_tcp_address());
    let (origin, release) = spawn_streaming_origin();
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("running", running)],
            relay: None,
            origin: Some(origin),
            body: "v1",
        },
        Duration::from_secs(10),
    );
    let mut in_flight = start_streaming_request(running);

    write_reload_config(
        &path,
        &ReloadShape {
            http: &[
                ("running", running),
                ("blocked", blocked_by_another_process),
            ],
            relay: None,
            origin: None,
            body: "v2",
        },
    );
    let occupant = std::net::TcpListener::bind(blocked_by_another_process)
        .expect("another process owns the new address");
    let error = harness
        .start_reload(&path)
        .expect_err("a listener that cannot bind rejects the reload");
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
    assert!(
        error.to_string().contains("blocked"),
        "the failure names the listener: {error}"
    );

    // Nothing moved: the master is idle on the old revision and its request is still in flight.
    assert_eq!(harness.master.state(), MasterState::Running);
    assert!(harness.pending.is_none());
    // A worker reports its changed counters whenever its status check is due, so only a lifecycle event is a move.
    let events = harness.master.poll(Instant::now()).expect("master poll");
    assert!(
        events
            .iter()
            .all(|event| matches!(event, MasterEvent::WorkerStatusUpdated { .. })),
        "{events:?}"
    );
    release.send(()).expect("release the origin");
    finish_streaming_request(&mut in_flight);
    drop(in_flight);
    assert_eq!(harness.master.state(), MasterState::Running);

    // Once the address is free the same revision applies.
    drop(occupant);
    harness
        .start_reload(&path)
        .expect("retry after the address is free");
    harness.wait_committed();
    assert_eq!(fresh_body(blocked_by_another_process), "v2");
    assert_eq!(fresh_body(running), "v2");
    harness.wait_retired_worker_exit();
    finish_reload_harness(harness);
}

#[test]
pub(super) fn reload_that_fails_in_the_candidate_rolls_back_and_releases_the_added_listener() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let (running, added) = (reserve_tcp_address(), reserve_tcp_address());
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("running", running)],
            relay: None,
            origin: None,
            body: "v1",
        },
        Duration::from_secs(10),
    );
    write_reload_config(
        &path,
        &ReloadShape {
            http: &[("running", running), ("added", added)],
            relay: None,
            origin: None,
            body: "v2",
        },
    );
    // A candidate that is told a different revision than the file holds rejects its configuration.
    let generation = harness.next_generation;
    harness.next_generation += 1;
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("reload configuration was rejected");
    };
    let reservations =
        ListenerReservations::prepare(&document.validated_config, Some(&harness.reservations))
            .expect("bind the added listener");
    let listeners = reservations
        .clone()
        .into_stable_listeners(&document.validated_config)
        .expect("stable listeners");
    let token = [u8::try_from(0x50 + generation).expect("token"); 16];
    let identity = WorkerIdentity {
        instance: InstanceToken(token),
        generation: GenerationId(generation),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
        .expect("real oxiroute worker")
        .arg(MARKER)
        .arg(generation.to_string())
        .arg(encode_token(token))
        .arg(&path)
        .arg("0".repeat(64));
    let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
        .expect("production launcher implementation");
    harness
        .master
        .replace_with_listeners(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-reload-rejected").expect("identity"),
                identity,
                command,
            },
            listeners,
            Instant::now(),
        )
        .expect("start rejected replacement");
    drop(reservations);
    harness.poll_events_until(|event| matches!(event, MasterEvent::RollbackCompleted { .. }));
    harness.poll_until(MasterState::Running);

    assert_eq!(fresh_body(running), "v1");
    assert_can_bind_again(added);
    finish_reload_harness(harness);
}

#[test]
pub(super) fn reload_keeps_a_long_lived_relay_connection_alive_past_the_old_phase_limit() {
    let directory = tempfile::tempdir().expect("reload fixture directory");
    let path = directory.path().join("oxiroute.kdl");
    let upstream = spawn_echo_upstream();
    let (web, relay, moved_relay) = (
        reserve_tcp_address(),
        reserve_tcp_address(),
        reserve_tcp_address(),
    );
    let mut harness = launch_reload_harness(
        &path,
        &ReloadShape {
            http: &[("web", web)],
            relay: Some(("relay", relay, upstream)),
            origin: None,
            body: "v1",
        },
        Duration::from_mins(2),
    );
    let mut stream = TcpStream::connect(relay).expect("relay connection");
    echo(&mut stream, b"before the reload").expect("echo before the reload");

    write_reload_config(
        &path,
        &ReloadShape {
            http: &[("web", web)],
            relay: Some(("relay", moved_relay, upstream)),
            origin: None,
            body: "v2",
        },
    );
    harness.start_reload(&path).expect("move the relay");
    harness.wait_committed();
    assert_eq!(fresh_body(web), "v2");

    // The relay connection outlives the nine second lifecycle limit of the previous design and the
    // ten second master phase limits; the retired worker is still serving it.
    let started = Instant::now();
    let mut rounds = 0_u32;
    while started.elapsed() < Duration::from_secs(12) {
        let events = harness.master.poll(Instant::now()).expect("master poll");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, MasterEvent::ReplacementCompleted { .. })),
            "the retired worker exited while its relay connection was open: {events:?}"
        );
        rounds += 1;
        if rounds.is_multiple_of(100) {
            echo(&mut stream, b"during the drain").expect("echo during the drain");
        }
        thread::sleep(POLL_INTERVAL * 4);
    }
    assert!(rounds >= 100, "the drain was not observed long enough");
    assert_eq!(harness.master.state(), MasterState::DrainingRetired);
    echo(&mut stream, b"after the drain window").expect("echo after twelve seconds");

    drop(stream);
    harness.wait_retired_worker_exit();
    let mut moved = TcpStream::connect(moved_relay).expect("moved relay connection");
    echo(&mut moved, b"through the new listener").expect("echo through the new worker");
    drop(moved);
    finish_reload_harness(harness);
}
