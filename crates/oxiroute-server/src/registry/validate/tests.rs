use super::*;

fn policy() -> PoolRegistration {
    PoolRegistration {
        allowed_cidrs: vec!["192.0.2.0/24".into(), "127.0.0.0/8".into()],
        ..PoolRegistration::default()
    }
}

fn request(address: &str) -> RegistrationRequest {
    RegistrationRequest {
        address: Some(address.parse().expect("address")),
        ..RegistrationRequest::default()
    }
}

fn put(address: &str, peer: Option<&str>) -> Result<Registration, RegistryError> {
    let requester = Requester {
        id: "token-a",
        peer: peer.map(|peer| peer.parse().expect("peer")),
    };
    validate_put(
        "render",
        "gpu-1",
        &policy(),
        &["192.0.2.1:8080".parse().expect("protected")],
        &requester,
        request(address),
    )
}

#[test]
fn a_minimal_request_gets_pool_defaults() {
    let record = put("192.0.2.10:9000", None).expect("valid");

    assert_eq!(
        (record.weight, record.priority, record.ttl_ms),
        (1, 100, 15_000)
    );
    assert_eq!(record.registered_by, "token-a");
    assert_eq!(record.announced_state, AnnouncedState::Ready);
}

#[test]
fn names_are_a_safe_path_segment() {
    for name in ["", "a b", "a/b", "a%2fb", "é", &"x".repeat(65)] {
        assert_eq!(
            validate_name(name),
            Err(RegistryError::InvalidName),
            "{name}"
        );
    }
    assert!(validate_name("gpu-1.node_a").is_ok());
}

#[test]
fn addresses_must_be_unicast_in_policy_and_not_oxiroute_itself() {
    let invalid = |address| matches!(put(address, None), Err(RegistryError::InvalidAddress(_)));
    assert!(invalid("0.0.0.0:9000"));
    assert!(invalid("192.0.2.10:0"));
    assert!(invalid("224.0.0.1:9000"));
    let denied = |address| matches!(put(address, None), Err(RegistryError::AddressNotAllowed(_)));
    assert!(denied("198.51.100.5:9000"));
    assert!(denied("192.0.2.1:8080"));
    assert!(put("127.0.0.1:9000", None).is_ok());
}

#[test]
fn loopback_and_link_local_need_an_explicit_cidr() {
    let mut open = policy();
    open.allowed_cidrs.clear();
    let requester = Requester {
        id: "t",
        peer: None,
    };
    let check = |address: &str, open: &PoolRegistration| {
        validate_put("render", "gpu", open, &[], &requester, request(address))
    };

    assert!(matches!(
        check("127.0.0.1:9000", &open),
        Err(RegistryError::AddressNotAllowed(_))
    ));
    assert!(matches!(
        check("169.254.1.1:9000", &open),
        Err(RegistryError::AddressNotAllowed(_))
    ));
    assert!(check("203.0.113.9:9000", &open).is_ok());
}

#[test]
fn tcp_peers_may_only_register_themselves_when_required() {
    assert!(put("192.0.2.10:9000", Some("192.0.2.10")).is_ok());
    assert!(matches!(
        put("192.0.2.10:9000", Some("192.0.2.99")),
        Err(RegistryError::AddressNotAllowed(_))
    ));
}

#[test]
fn ttl_weight_labels_and_metadata_are_bounded() {
    let requester = Requester {
        id: "t",
        peer: None,
    };
    let with = |change: &dyn Fn(&mut RegistrationRequest)| {
        let mut request = request("192.0.2.10:9000");
        change(&mut request);
        validate_put("render", "gpu", &policy(), &[], &requester, request)
    };

    assert_eq!(
        with(&|r| r.ttl_ms = Some(10)),
        Err(RegistryError::InvalidTtl)
    );
    assert_eq!(
        with(&|r| r.ttl_ms = Some(u64::MAX)),
        Err(RegistryError::InvalidTtl)
    );
    assert!(matches!(
        with(&|r| r.weight = Some(0)),
        Err(RegistryError::InvalidRequest(_))
    ));
    assert!(matches!(
        with(&|r| r.weight = Some(101)),
        Err(RegistryError::InvalidRequest(_))
    ));
    assert!(matches!(
        with(&|r| r.max_connections = Some(0)),
        Err(RegistryError::InvalidRequest(_))
    ));
    assert_eq!(
        with(&|r| r.labels = (0..17).map(|i| (format!("k{i}"), String::new())).collect()),
        Err(RegistryError::InvalidMetadata)
    );
    assert_eq!(
        with(&|r| r.metadata = serde_json::Value::String("x".repeat(3000))),
        Err(RegistryError::InvalidMetadata)
    );
}

#[test]
fn a_heartbeat_can_announce_draining_and_extend_the_ttl() {
    let record = put("192.0.2.10:9000", None).expect("valid");

    let updated = apply_heartbeat(
        &policy(),
        record,
        HeartbeatRequest {
            state: Some(AnnouncedState::Draining),
            ttl_ms: Some(30_000),
            ..HeartbeatRequest::default()
        },
    )
    .expect("heartbeat");

    assert_eq!(updated.announced_state, AnnouncedState::Draining);
    assert_eq!(updated.ttl_ms, 30_000);
    assert_eq!(updated.effective_state(), AdministrativeState::Drain);
}
