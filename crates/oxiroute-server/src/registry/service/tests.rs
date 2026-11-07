use oxiroute_config::{HealthStartup, UpstreamAlgorithm};
use tempfile::TempDir;

use super::*;
use crate::{
    registry::{auth::Scope, lease::ManualClock},
    routing::RuntimeServer,
};

struct Harness {
    dir: TempDir,
    clock: Arc<ManualClock>,
    service: Arc<RegistryService>,
}

fn policy() -> PoolRegistration {
    PoolRegistration {
        allowed_cidrs: vec!["192.0.2.0/24".into()],
        require_peer_address: false,
        max_servers: 3,
        ..PoolRegistration::default()
    }
}

fn pool(name: &str, statics: &[(&str, u16)]) -> Arc<RoundRobinPool> {
    Arc::new(
        RoundRobinPool::new_named_servers(
            name.into(),
            statics.iter().map(|(server, port)| RuntimeServer {
                name: (*server).into(),
                endpoint: RuntimeEndpoint::from(SocketAddr::from(([192, 0, 2, 200], *port))),
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            }),
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool"),
    )
}

fn binding(pool: &Arc<RoundRobinPool>, policy: Option<PoolRegistration>) -> PoolBinding {
    PoolBinding::new(pool, policy, None, Arc::from([]))
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("dir");
    let clock = Arc::new(ManualClock::new());
    let service =
        RegistryService::with_store(Store::open(dir.path()).expect("store"), clock.clone(), None);
    service.set_writer(true);
    Harness {
        dir,
        clock,
        service,
    }
}

fn admin() -> Principal {
    Principal::admin("root")
}

fn registrar(id: &str, pools: &[&str]) -> Principal {
    Principal {
        id: id.into(),
        scope: Scope::Register {
            pools: pools.iter().map(|pool| (*pool).to_owned()).collect(),
            name_prefix: None,
        },
    }
}

fn request(address: &str) -> RegistrationRequest {
    RegistrationRequest {
        address: Some(address.parse().expect("address")),
        ttl_ms: Some(15_000),
        ..RegistrationRequest::default()
    }
}

fn put(
    h: &Harness,
    who: &Principal,
    pool: &str,
    name: &str,
    address: &str,
) -> Result<PutOutcome, RegistryError> {
    h.service
        .put(who, pool, name, request(address), None, false)
}

fn members(pool: &RoundRobinPool) -> Vec<String> {
    pool.health_snapshot()
        .endpoints
        .into_iter()
        .map(|member| member.name)
        .collect()
}

#[test]
fn a_registered_server_joins_the_pool_and_a_repeat_put_only_updates() {
    let h = harness();
    let render = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&render, Some(policy()))]);

    let created = put(&h, &admin(), "render", "gpu-1", "192.0.2.10:9000").expect("created");
    let updated = put(&h, &admin(), "render", "gpu-1", "192.0.2.10:9000").expect("updated");

    assert!(created.created && !updated.created);
    assert_eq!(members(&render), ["local", "gpu-1"]);
    assert_eq!(
        render.health_snapshot().endpoints[1].origin,
        ServerOrigin::Dynamic
    );
    assert_eq!(
        updated.registry_revision, created.registry_revision,
        "an identical PUT does not rewrite the store"
    );
}

#[test]
fn an_unrenewed_lease_expires_and_a_heartbeat_keeps_it() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    put(&h, &admin(), "render", "kept", "192.0.2.10:9000").expect("kept");
    put(&h, &admin(), "render", "lost", "192.0.2.11:9000").expect("lost");
    let in_flight = render.select().expect("lease on a member");

    h.clock.advance(Duration::from_secs(10));
    h.service
        .heartbeat(&admin(), "render", "kept", HeartbeatRequest::default())
        .expect("renew");
    h.clock.advance(Duration::from_secs(10));
    h.service.tick();

    assert_eq!(members(&render), ["kept"]);
    assert_eq!(
        h.service.list(None).expect("list").len(),
        1,
        "expiry is persisted"
    );
    assert_eq!(h.service.metrics().expirations["render"], 1);
    drop(in_flight);
}

#[test]
fn heartbeats_do_not_write_but_a_changed_state_does() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    let created = put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");
    h.service
        .heartbeat(&admin(), "render", "gpu", HeartbeatRequest::default())
        .expect("beat");
    assert_eq!(h.service.status().revision, created.registry_revision);

    let draining = HeartbeatRequest {
        state: Some(crate::registry::record::AnnouncedState::Draining),
        ..HeartbeatRequest::default()
    };
    h.service
        .heartbeat(&admin(), "render", "gpu", draining)
        .expect("draining");

    assert_eq!(h.service.status().revision, created.registry_revision + 1);
    assert_eq!(
        render.health_snapshot().endpoints[0].administrative_state,
        AdministrativeState::Drain
    );
}

#[test]
fn unknown_names_ask_the_worker_to_register_again() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);

    assert_eq!(
        h.service
            .heartbeat(&admin(), "render", "ghost", HeartbeatRequest::default()),
        Err(RegistryError::RegistrationNotFound)
    );
    assert_eq!(
        h.service.delete(&admin(), "render", "ghost"),
        Err(RegistryError::RegistrationNotFound)
    );
}

#[test]
fn deregistering_removes_the_server_at_once() {
    let h = harness();
    let render = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");

    h.service
        .delete(&admin(), "render", "gpu")
        .expect("deleted");

    assert_eq!(members(&render), ["local"]);
    assert!(h.service.list(None).expect("list").is_empty());
}

#[test]
fn conflicts_with_static_servers_other_holders_and_limits_are_refused() {
    let h = harness();
    let render = pool("render", &[("local", 3000)]);
    let closed = pool("closed", &[("one", 3001)]);
    h.service.attach(vec![
        binding(&render, Some(policy())),
        binding(&closed, None),
    ]);
    put(
        &h,
        &registrar("a", &["render"]),
        "render",
        "gpu",
        "192.0.2.10:9000",
    )
    .expect("a registers");

    let a = registrar("a", &["render"]);
    let b = registrar("b", &["render"]);
    assert_eq!(
        put(&h, &a, "render", "local", "192.0.2.50:9000"),
        Err(RegistryError::NameConflict)
    );
    assert_eq!(
        put(&h, &a, "render", "other", "192.0.2.10:9000"),
        Err(RegistryError::EndpointConflict)
    );
    assert_eq!(
        put(&h, &b, "render", "gpu", "192.0.2.77:9000"),
        Err(RegistryError::LeaseHeld)
    );
    assert!(
        h.service
            .put(
                &admin(),
                "render",
                "gpu",
                request("192.0.2.77:9000"),
                None,
                true
            )
            .is_ok(),
        "an admin may take over"
    );
    put(&h, &a, "render", "two", "192.0.2.11:9000").expect("second");
    put(&h, &a, "render", "three", "192.0.2.12:9000").expect("third");
    assert_eq!(
        put(&h, &a, "render", "four", "192.0.2.13:9000"),
        Err(RegistryError::PoolFull)
    );
    assert_eq!(
        put(&h, &a, "closed", "x", "192.0.2.14:9000"),
        Err(RegistryError::ScopeDenied)
    );
    assert_eq!(
        put(&h, &admin(), "closed", "x", "192.0.2.14:9000"),
        Err(RegistryError::PoolNotDynamic)
    );
    assert_eq!(
        put(&h, &admin(), "missing", "x", "192.0.2.14:9000"),
        Err(RegistryError::PoolNotFound)
    );
    assert_eq!(members(&render).len(), 4);
}

#[test]
fn a_replaced_address_moves_the_server_and_keeps_the_operator_drain() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");
    h.service
        .set_operator_state(&admin(), "render", "gpu", AdministrativeState::Drain)
        .expect("drain");

    put(&h, &admin(), "render", "gpu", "192.0.2.20:9000").expect("moved");

    let member = render.health_snapshot().endpoints.remove(0);
    assert_eq!(
        member.address,
        RuntimeEndpoint::from(SocketAddr::from(([192, 0, 2, 20], 9000)))
    );
    assert_eq!(member.administrative_state, AdministrativeState::Drain);
}

#[test]
fn a_new_generation_is_seeded_with_registered_servers_and_their_drain() {
    let h = harness();
    let first = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&first, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");
    h.service
        .set_operator_state(&admin(), "render", "gpu", AdministrativeState::Maintenance)
        .expect("maintenance");

    let second = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&second, Some(policy()))]);

    assert_eq!(members(&second), ["local", "gpu"]);
    assert_eq!(
        second.health_snapshot().endpoints[1].administrative_state,
        AdministrativeState::Maintenance
    );
    put(&h, &admin(), "render", "late", "192.0.2.11:9000").expect("late");
    assert!(
        members(&first).contains(&"late".to_owned()),
        "the retiring generation follows too"
    );
    assert!(members(&second).contains(&"late".to_owned()));
}

#[test]
fn records_of_a_pool_that_stops_registering_are_kept_and_reactivate() {
    let h = harness();
    let first = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&first, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");

    let closed = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&closed, None)]);
    assert_eq!(members(&closed), ["local"]);
    assert_eq!(
        h.service.list(None).expect("list")[0].status,
        RegistrationStatus::Orphaned
    );

    let reopened = pool("render", &[("local", 3000)]);
    h.service.attach(vec![binding(&reopened, Some(policy()))]);
    assert_eq!(members(&reopened), ["local", "gpu"]);
    assert_eq!(
        h.service.list(None).expect("list")[0].status,
        RegistrationStatus::Active
    );
}

#[test]
fn a_new_static_server_shadows_a_registration_without_rejecting_either() {
    let h = harness();
    let first = pool("render", &[]);
    h.service.attach(vec![binding(&first, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");

    let second = pool("render", &[("gpu", 3000)]);
    h.service.attach(vec![binding(&second, Some(policy()))]);

    assert_eq!(members(&second), ["gpu"]);
    assert_eq!(
        second.health_snapshot().endpoints[0].origin,
        ServerOrigin::Static
    );
    assert_eq!(
        h.service.list(None).expect("list")[0].status,
        RegistrationStatus::Shadowed
    );
}

#[test]
fn a_restart_restores_members_with_a_full_lease() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");
    h.clock.advance(Duration::from_secs(14));
    drop(h.service);

    let restarted = RegistryService::with_store(
        Store::open(h.dir.path()).expect("store"),
        h.clock.clone(),
        None,
    );
    restarted.set_writer(true);
    let reborn = pool("render", &[]);
    restarted.attach(vec![binding(&reborn, Some(policy()))]);

    assert_eq!(members(&reborn), ["gpu"]);
    h.clock.advance(Duration::from_secs(14));
    restarted.tick();
    assert_eq!(members(&reborn), ["gpu"], "the restart granted a fresh TTL");
    h.clock.advance(Duration::from_secs(2));
    restarted.tick();
    assert_eq!(members(&reborn), Vec::<String>::new());
}

#[test]
fn only_the_writer_changes_registrations_and_followers_mirror_it() {
    let h = harness();
    let writer_pool = pool("render", &[]);
    h.service
        .attach(vec![binding(&writer_pool, Some(policy()))]);
    let follower = RegistryService::with_store(
        Store::open(h.dir.path()).expect("store"),
        h.clock.clone(),
        None,
    );
    let follower_pool = pool("render", &[]);
    follower.attach(vec![binding(&follower_pool, Some(policy()))]);

    assert_eq!(
        follower
            .put(
                &admin(),
                "render",
                "gpu",
                request("192.0.2.10:9000"),
                None,
                false
            )
            .map(|_| ()),
        Err(RegistryError::GenerationInactive)
    );
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("created");
    follower.tick();
    assert_eq!(members(&follower_pool), ["gpu"]);

    h.clock.advance(Duration::from_secs(20));
    h.service.tick();
    follower.tick();
    assert!(
        members(&follower_pool).is_empty(),
        "expiry reaches the follower through the store"
    );
}

#[test]
fn two_pools_register_independently_with_pool_scoped_tokens() {
    let h = harness();
    let render = pool("render", &[]);
    let voice = pool("voice", &[("local", 3000)]);
    h.service.attach(vec![
        binding(&render, Some(policy())),
        binding(&voice, Some(policy())),
    ]);
    let voice_agent = registrar("voice-agent", &["voice"]);

    put(&h, &voice_agent, "voice", "tts-1", "192.0.2.30:9100").expect("voice registers");
    assert_eq!(
        put(&h, &voice_agent, "render", "tts-1", "192.0.2.30:9100"),
        Err(RegistryError::ScopeDenied)
    );
    put(&h, &admin(), "render", "gpu-1", "192.0.2.10:9000").expect("render registers");

    assert_eq!(members(&voice), ["local", "tts-1"]);
    assert_eq!(members(&render), ["gpu-1"]);
    h.service
        .delete(&voice_agent, "voice", "tts-1")
        .expect("deregister");
    assert_eq!(members(&voice), ["local"]);
    assert_eq!(members(&render), ["gpu-1"]);
}

#[cfg(unix)]
#[test]
fn an_unwritable_store_refuses_changes_and_leaves_the_pool_untouched() {
    use std::os::unix::fs::PermissionsExt as _;
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    std::fs::set_permissions(h.dir.path(), std::fs::Permissions::from_mode(0o500))
        .expect("read-only");
    if std::fs::File::create(h.dir.path().join("probe")).is_ok() {
        return;
    }

    let result = put(&h, &admin(), "render", "gpu", "192.0.2.10:9000");

    std::fs::set_permissions(h.dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("restore");
    assert_eq!(result.map(|_| ()), Err(RegistryError::RegistryUnavailable));
    assert_eq!(members(&render), Vec::<String>::new());
    assert!(h.service.status().degraded.is_some());
}

fn server_updates() -> usize {
    crate::operational_event::page(0, 1000)
        .events
        .iter()
        .filter(|event| event.event.as_str() == "server_update")
        .count()
}

#[test]
fn joins_changes_departures_and_expiries_are_events_but_renewals_are_not() {
    let h = harness();
    let render = pool("render", &[]);
    h.service.attach(vec![binding(&render, Some(policy()))]);
    let before = server_updates();

    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("joined");
    assert_eq!(server_updates(), before + 1, "a join");
    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("renewed by PUT");
    h.service
        .heartbeat(&admin(), "render", "gpu", HeartbeatRequest::default())
        .expect("renewed");
    assert_eq!(server_updates(), before + 1, "renewals stay silent");

    h.service
        .delete(&admin(), "render", "gpu")
        .expect("deleted");
    assert_eq!(server_updates(), before + 2, "a departure");

    put(&h, &admin(), "render", "gpu", "192.0.2.10:9000").expect("joined again");
    h.clock.advance(Duration::from_secs(30));
    h.service.tick();
    assert_eq!(server_updates(), before + 4, "a join and an expiry");
}
