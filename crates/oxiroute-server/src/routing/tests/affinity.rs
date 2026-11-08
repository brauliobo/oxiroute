#[allow(clippy::wildcard_imports)]
use std::fmt::Write as _;

use super::*;

const SERVERS: usize = 6;

fn pool() -> EndpointPool {
    EndpointPool::new_named_servers(
        "sticky".into(),
        (0..SERVERS).map(|index| {
            runtime_server(
                &format!("s{index}"),
                4000 + u16::try_from(index).expect("small"),
                None,
            )
        }),
        UpstreamAlgorithm::RoundRobin,
        Some(HealthStartup::Healthy),
        None,
    )
    .expect("pool")
}

fn owners(pool: &EndpointPool) -> Vec<Option<String>> {
    (0..500)
        .map(|key| pool.rendezvous_server(format!("session-{key}").as_bytes()))
        .collect()
}

#[test]
fn cookie_ids_are_the_published_hash_of_the_pool_and_server_names() {
    let pool = pool();
    let digest = openssl::sha::sha256(b"oxiroute-affinity\0sticky\0s3");
    let expected = digest[..8].iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    assert_eq!(
        format!("{:016x}", pool.cookie_id("s3").expect("id")),
        expected
    );
    assert_eq!(pool.cookie_id("missing"), None);
}

#[test]
fn a_cookie_id_names_its_server_only_while_that_server_is_selectable() {
    let pool = pool();
    let id = pool.cookie_id("s4").expect("id");
    assert_eq!(pool.server_with_cookie_id(id).as_deref(), Some("s4"));
    pool.set_server_administrative_state("s4", AdministrativeState::Drain)
        .expect("drain");
    assert_eq!(pool.server_with_cookie_id(id), None);
    pool.set_server_administrative_state("s4", AdministrativeState::Ready)
        .expect("ready");
    assert_eq!(pool.server_with_cookie_id(id).as_deref(), Some("s4"));
    assert_eq!(pool.server_with_cookie_id(id ^ 1), None);
}

#[test]
fn a_named_server_is_preferred_only_while_it_is_selectable() {
    let pool = pool();
    assert_eq!(pool.selectable_server("s1").as_deref(), Some("s1"));
    pool.set_server_health_override("s1", HealthOverride::Down)
        .expect("down");
    assert_eq!(pool.selectable_server("s1"), None);
    assert_eq!(pool.selectable_server("missing"), None);
}

#[test]
fn rendezvous_keys_stay_put_until_their_own_server_leaves() {
    let pool = pool();
    let before = owners(&pool);
    assert_eq!(before, owners(&pool));
    assert!(before.iter().any(|owner| owner.as_deref() == Some("s2")));

    pool.set_server_administrative_state("s2", AdministrativeState::Drain)
        .expect("drain");
    let after = owners(&pool);
    for (old, new) in before.iter().zip(&after) {
        assert!(
            old == new || old.as_deref() == Some("s2"),
            "{old:?} moved to {new:?}"
        );
    }
    assert!(after.iter().all(|owner| owner.as_deref() != Some("s2")));

    pool.set_server_administrative_state("s2", AdministrativeState::Ready)
        .expect("ready");
    assert_eq!(owners(&pool), before);
}

#[test]
fn a_runtime_weight_moves_the_share_of_keys() {
    let pool = pool();
    let share = |pool: &EndpointPool| {
        owners(pool)
            .iter()
            .filter(|owner| owner.as_deref() == Some("s0"))
            .count()
    };
    let even = share(&pool);
    pool.set_server_weight("s0", Some(100)).expect("weight");
    let heavy = share(&pool);
    pool.set_server_weight("s0", None).expect("reset");
    assert!(heavy > 400 && even < 150, "even {even}, heavy {heavy}");
    assert_eq!(share(&pool), even);
}

#[test]
fn nothing_is_preferred_in_a_pool_without_selectable_servers() {
    let pool = pool();
    pool.set_administrative_state(AdministrativeState::Drain);
    assert!(owners(&pool).iter().all(Option::is_none));
}

#[test]
fn a_registered_servers_new_weight_moves_its_share_without_a_membership_change() {
    let pool = pool();
    let dynamic = |weight| DynamicServer {
        weight,
        priority: 100,
    };
    pool.add_server(runtime_server("late", 5000, None), dynamic(1))
        .expect("add");
    let share = |pool: &EndpointPool| {
        owners(pool)
            .iter()
            .filter(|owner| owner.as_deref() == Some("late"))
            .count()
    };
    let even = share(&pool);
    pool.update_server("late", dynamic(100), None)
        .expect("heavier");
    let heavy = share(&pool);
    pool.update_server("late", dynamic(1), None).expect("reset");
    assert!(heavy > 400 && even < 150, "even {even}, heavy {heavy}");
    assert_eq!(share(&pool), even);
}
