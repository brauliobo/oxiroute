use super::*;

#[test]
fn normalized_nginx_hosts_accept_one_trailing_dot() {
    let authority = "API.EXAMPLE.TEST.:443"
        .parse::<Authority>()
        .expect("trailing-dot authority");

    assert_eq!(
        normalized_authority_host(authority.as_str()).as_deref(),
        Some("api.example.test")
    );
}

#[test]
fn dns_addresses_are_ordered_deterministically_for_every_consumer() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let first = SocketAddr::from(([192, 0, 2, 1], 443));
    let second = SocketAddr::from(([192, 0, 2, 2], 443));

    let traffic_addresses = endpoint
        .order_addresses([second, first, second])
        .expect("traffic addresses");
    let health_addresses = endpoint
        .order_addresses([first, second])
        .expect("health addresses");

    assert_eq!(traffic_addresses, vec![first, second]);
    assert_eq!(health_addresses, traffic_addresses);
}

#[test]
fn dns_resolution_rejects_an_empty_address_set() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };

    let error = endpoint
        .order_addresses([])
        .expect_err("empty DNS resolution must fail");

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains("origin.example.test:443"));
}

#[test]
fn dns_resolution_accepts_the_normalized_address_limit() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let expected = resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES);
    let input = expected
        .iter()
        .rev()
        .copied()
        .chain(expected.iter().copied());

    let addresses = endpoint
        .order_addresses(input)
        .expect("address limit is accepted");

    assert_eq!(addresses, expected);
}

#[test]
fn dns_resolution_rejects_normalized_address_overflow() {
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };

    let error = endpoint
        .order_addresses(resolved_addresses(MAX_RESOLVED_ENDPOINT_ADDRESSES + 1))
        .expect_err("address overflow must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("returned more than 16 addresses")
    );
}

fn resolved_addresses(count: usize) -> Vec<SocketAddr> {
    (1..=count)
        .map(|index| {
            SocketAddr::from((
                [192, 0, 2, u8::try_from(index).expect("test address octet")],
                443,
            ))
        })
        .collect()
}

#[tokio::test]
async fn startup_dns_uses_only_the_addresses_pinned_in_the_runtime_plan() {
    let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
    let pool = RoundRobinPool::new_named_servers(
        "pinned".into(),
        [RuntimeServer {
            name: "origin".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "origin.example.test".into(),
                port: 443,
            },
            max_connections: None,
            pinned_addresses: Some(vec![pinned].into()),
            protected_addresses: Arc::from([]),
        }],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("startup-pinned pool");

    let server = pool.select().expect("pinned server");
    assert_eq!(
        server.resolve_addresses().await.expect("pinned addresses"),
        vec![pinned]
    );
    assert_eq!(server.endpoint().to_string(), "origin.example.test:443");
}

#[tokio::test]
async fn dns_refresh_resolution_does_not_mutate_pinned_addresses_before_commit() {
    let pinned = SocketAddr::from(([192, 0, 2, 10], 443));
    let pool = RoundRobinPool::new_named_servers(
        "pinned".into(),
        [RuntimeServer {
            name: "origin".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "localhost".into(),
                port: 443,
            },
            max_connections: None,
            pinned_addresses: Some(vec![pinned].into()),
            protected_addresses: Arc::from([]),
        }],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("startup-pinned pool");

    let resolved = pool
        .resolve_server_dns("origin")
        .await
        .expect("external DNS resolution");
    let before_commit = pool.select().expect("server before commit");
    assert_eq!(
        before_commit
            .resolve_addresses()
            .await
            .expect("pinned address"),
        vec![pinned]
    );
    drop(before_commit);

    pool.commit_server_dns("origin", &resolved)
        .expect("atomic DNS commit");
    let after_commit = pool.select().expect("server after commit");
    assert_eq!(
        after_commit
            .resolve_addresses()
            .await
            .expect("committed addresses"),
        resolved
    );
}

#[tokio::test]
async fn on_connect_dns_rejects_a_protected_listener_after_resolution() {
    let protected = SocketAddr::from(([127, 0, 0, 1], 18404));
    let pool = RoundRobinPool::new_named_servers(
        "protected".into(),
        [RuntimeServer {
            name: "rebind".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "localhost".into(),
                port: protected.port(),
            },
            max_connections: None,
            pinned_addresses: None,
            protected_addresses: Arc::from([protected]),
        }],
        UpstreamAlgorithm::RoundRobin,
        None,
        None,
    )
    .expect("protected DNS pool");

    let lease = pool.select().expect("selected DNS server");
    let error = lease
        .resolve_addresses()
        .await
        .expect_err("protected address must be rejected after DNS resolution");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

fn on_connect_pool(host: &str) -> RoundRobinPool {
    RoundRobinPool::new_named_servers(
        "on-connect".into(),
        [RuntimeServer {
            name: "origin".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: host.into(),
                port: 443,
            },
            max_connections: None,
            pinned_addresses: None,
            protected_addresses: Arc::from([]),
        }],
        UpstreamAlgorithm::First,
        None,
        None,
    )
    .expect("on-connect pool")
}

#[tokio::test]
async fn on_connect_dns_requests_share_the_latest_lookup() {
    let pool = on_connect_pool("localhost");
    let server = pool.handle(0).0;
    assert!(server.resolved.fresh().is_none());

    let first = pool.select().expect("lease").resolve_addresses().await;
    let cached = server
        .resolved
        .fresh()
        .expect("lookup kept for the next request");
    let second = pool.select().expect("lease").resolve_addresses().await;

    assert_eq!(first.expect("first"), cached.to_vec());
    assert_eq!(second.expect("second"), cached.to_vec());
    assert!(Arc::ptr_eq(
        &cached,
        &server.resolved.fresh().expect("unchanged")
    ));
}

#[tokio::test]
async fn a_failed_connection_makes_the_next_request_resolve_again() {
    let pool = on_connect_pool("localhost");
    let server = pool.handle(0).0;
    pool.select()
        .expect("lease")
        .resolve_addresses()
        .await
        .expect("lookup");

    pool.record_passive_failure(0, HealthFailure::UnexpectedStatus);
    assert!(
        server.resolved.fresh().is_some(),
        "an error status says nothing about the address"
    );
    pool.record_passive_failure(0, HealthFailure::ConnectFailed);

    assert!(server.resolved.fresh().is_none());
}

#[tokio::test]
async fn health_probes_look_up_fresh_and_refresh_the_shared_lookup() {
    let pool = on_connect_pool("localhost");
    let server = pool.handle(0).0;

    let probed = pool
        .resolve_server_addresses(&pool.server_handle("origin").expect("handle"))
        .await
        .expect("probe lookup");

    assert_eq!(server.resolved.fresh().expect("stored").to_vec(), probed);
}

#[tokio::test]
async fn a_protected_dns_answer_is_never_kept_for_later_requests() {
    let protected = SocketAddr::from(([127, 0, 0, 1], 18404));
    let pool = RoundRobinPool::new_named_servers(
        "protected".into(),
        [RuntimeServer {
            name: "rebind".into(),
            endpoint: RuntimeEndpoint::Dns {
                host: "localhost".into(),
                port: protected.port(),
            },
            max_connections: None,
            pinned_addresses: None,
            protected_addresses: Arc::from([protected]),
        }],
        UpstreamAlgorithm::RoundRobin,
        None,
        None,
    )
    .expect("protected DNS pool");

    for _ in 0..2 {
        let error = pool
            .select()
            .expect("lease")
            .resolve_addresses()
            .await
            .expect_err("rejected on every request");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    assert!(pool.handle(0).0.resolved.fresh().is_none());
}
