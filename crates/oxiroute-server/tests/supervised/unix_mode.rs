use super::reload::{finish_reload_harness, fresh_body, reload_config, spawn_echo_upstream};
#[allow(clippy::wildcard_imports)]
use super::*;

fn unix_relay_config(web: SocketAddr, path: &Path, mode: u16, upstream: SocketAddr) -> ConfigDraft {
    let mut config = reload_config(&ReloadShape {
        http: &[("web", web)],
        relay: Some(("relay", reserve_tcp_address(), upstream)),
        origin: None,
        body: "v1",
    });
    let relay = config
        .listeners
        .iter_mut()
        .find(|listener| listener.name == "relay")
        .expect("relay listener");
    relay.bind = ListenerBind::Unix {
        path: path.to_owned(),
        mode: Some(mode),
    };
    config
}

fn write_config(path: &Path, config: &ConfigDraft) {
    fs::write(
        path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid unix mode config"),
        ),
    )
    .expect("write unix mode config");
}

fn socket_mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o7777
}

fn unix_echo(stream: &mut UnixStream, message: &[u8]) {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .expect("unix read timeout");
    stream.write_all(message).expect("write to the relay");
    let mut echoed = vec![0_u8; message.len()];
    stream.read_exact(&mut echoed).expect("read from the relay");
    assert_eq!(echoed, message);
}

#[test]
pub(super) fn reload_applies_a_changed_unix_socket_mode_in_place_and_keeps_its_clients() {
    let directory = tempfile::tempdir().expect("unix mode fixture directory");
    let (path, socket) = (
        directory.path().join("oxiroute.kdl"),
        directory.path().join("relay.sock"),
    );
    let (web, upstream) = (reserve_tcp_address(), spawn_echo_upstream());
    write_config(&path, &unix_relay_config(web, &socket, 0o600, upstream));
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);
    let mut established = UnixStream::connect(&socket).expect("established relay client");
    unix_echo(&mut established, b"before the reload");

    let mut updated = unix_relay_config(web, &socket, 0o660, upstream);
    let HttpRouteAction::FixedResponse { body, .. } =
        &mut updated.http_services[0].routes[0].action
    else {
        panic!("fixture route is a fixed response");
    };
    *body = "v2".into();
    write_config(&path, &updated);
    let changes = harness.start_reload(&path).expect("reload with a new mode");
    assert!(!changes.changes_listeners(), "{changes:?}");
    harness.wait_committed();

    assert_eq!(socket_mode(&socket), 0o660);
    assert_eq!(fresh_body(web), "v2");
    unix_echo(&mut established, b"during the drain");
    let mut fresh = UnixStream::connect(&socket).expect("fresh relay client");
    unix_echo(&mut fresh, b"through the new worker");

    drop((established, fresh));
    harness.wait_retired_worker_exit();
    assert_eq!(socket_mode(&socket), 0o660);
    finish_reload_harness(harness);
}

#[test]
pub(super) fn a_rejected_replacement_restores_the_unix_socket_mode() {
    let directory = tempfile::tempdir().expect("unix mode fixture directory");
    let (path, socket) = (
        directory.path().join("oxiroute.kdl"),
        directory.path().join("relay.sock"),
    );
    let (web, upstream) = (reserve_tcp_address(), spawn_echo_upstream());
    write_config(&path, &unix_relay_config(web, &socket, 0o600, upstream));
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    harness.poll_until(MasterState::Running);

    write_config(&path, &unix_relay_config(web, &socket, 0o666, upstream));
    let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("reload configuration was rejected");
    };
    let reservations =
        ListenerReservations::prepare(&document.validated_config, Some(&harness.reservations))
            .expect("prepare the new mode");
    assert_eq!(socket_mode(&socket), 0o666);
    let listeners = reservations
        .clone()
        .into_stable_listeners(&document.validated_config)
        .expect("stable listeners");
    let generation = harness.next_generation;
    harness.next_generation += 1;
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
    let mut factory = worker_spawner();
    harness
        .master
        .replace_with_listeners(
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-mode-rejected").expect("identity"),
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

    assert_eq!(socket_mode(&socket), 0o600);
    let mut client = UnixStream::connect(&socket).expect("relay client after the rollback");
    unix_echo(&mut client, b"unchanged");
    drop(client);
    finish_reload_harness(harness);
}
