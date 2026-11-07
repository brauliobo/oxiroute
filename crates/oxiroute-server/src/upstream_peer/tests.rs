use std::{net::SocketAddr, sync::Arc, time::Duration};

use http::Version;
use oxiroute_config::{
    HttpVersion, HttpVersionPolicy, UpstreamAlgorithm, UpstreamEndpoint, UpstreamPool, UpstreamTls,
};
use pingora::{
    ErrorSource,
    listeners::ALPN,
    protocols::{Digest, tls::SslDigest},
    upstreams::peer::Peer,
};

use super::*;

#[tokio::test]
async fn endpoint_deadline_stops_address_fallback_with_a_non_retryable_timeout() {
    let first = SocketAddr::from(([192, 0, 2, 1], 443));
    let second = SocketAddr::from(([192, 0, 2, 2], 443));
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: 443,
    };
    let selector = Arc::new(
        RoundRobinPool::new_named(
            "deadline".into(),
            [endpoint],
            UpstreamAlgorithm::RoundRobin,
            false,
        )
        .expect("deadline selector"),
    );
    let mut selected = SelectedEndpoint::with_addresses(
        selector.select().expect("deadline lease"),
        vec![first, second],
    );
    let plan = UpstreamPlan::new(selector, None);
    let timeout = Duration::from_secs(1);

    let first_peer = selected
        .prepare_peer(&plan, timeout, timeout)
        .await
        .expect("first peer");
    assert_eq!(first_peer.address().as_inet(), Some(&first));
    selected.deadline = Some(Instant::now());

    assert!(!selected.has_address_fallback());
    let error = selected
        .prepare_peer(&plan, timeout, timeout)
        .await
        .expect_err("expired deadline must stop fallback");
    assert_eq!(error.etype(), &ErrorType::ConnectTimedout);
    assert_eq!(error.esource(), &ErrorSource::Upstream);
    assert!(!error.retry());
    assert_eq!(
        selected
            .addresses
            .as_ref()
            .expect("resolved addresses")
            .as_slice(),
        &[second]
    );
}

#[tokio::test]
async fn compiled_upstream_peer_retains_tls_policy_timeouts_and_reuse_isolation() {
    let timeout = Duration::from_secs(7);
    let address = SocketAddr::from(([127, 0, 0, 1], 443));
    let tls = upstream_tls_plan(
        "origin.example.test",
        HttpVersion::Http11,
        HttpVersion::Http2,
    );
    let plan = UpstreamPlan::new(
        Arc::new(RoundRobinPool::new([address]).expect("selector")),
        Some(Arc::clone(&tls)),
    );

    let peer = plan.peer(address, timeout, timeout);
    assert!(peer.is_tls());
    assert_eq!(peer.sni, "origin.example.test");
    assert!(peer.options.verify_cert);
    assert!(peer.options.verify_hostname);
    assert_eq!(peer.options.alpn, ALPN::H2H1);
    assert_eq!(peer.group_key, tls.group_key());
    assert_eq!(peer.options.connection_timeout, Some(timeout));
    assert_eq!(peer.options.total_connection_timeout, Some(timeout));
    assert_eq!(peer.options.read_timeout, Some(timeout));
    assert_eq!(peer.options.write_timeout, Some(timeout));
    assert_eq!(
        peer.reuse_hash(),
        plan.peer(address, timeout, timeout).reuse_hash()
    );

    let isolated_tls = upstream_tls_plan(
        "other.example.test",
        HttpVersion::Http11,
        HttpVersion::Http2,
    );
    let isolated = UpstreamPlan::new(Arc::clone(plan.selector()), Some(isolated_tls))
        .peer(address, timeout, timeout);
    assert_ne!(peer.reuse_hash(), isolated.reuse_hash());

    let plaintext =
        UpstreamPlan::new(Arc::clone(plan.selector()), None).peer(address, timeout, timeout);
    assert!(!plaintext.is_tls());
    assert!(plaintext.sni.is_empty());
    assert_eq!(plaintext.group_key, 0);
    assert_eq!(plaintext.options.alpn, ALPN::H1);
    assert_eq!(plaintext.options.connection_timeout, Some(timeout));
    assert_eq!(plaintext.options.total_connection_timeout, Some(timeout));
    assert_eq!(plaintext.options.read_timeout, Some(timeout));
    assert_eq!(plaintext.options.write_timeout, Some(timeout));
}

#[test]
fn pool_connect_server_timeouts_and_never_reuse_are_independent() {
    let address = SocketAddr::from(([127, 0, 0, 1], 8080));
    let selector = Arc::new(RoundRobinPool::new([address]).expect("selector"));
    let plan = UpstreamPlan::with_policy(
        selector,
        None,
        Some(Duration::from_secs(2)),
        Some(Duration::from_secs(11)),
        UpstreamConnectionReuse::Never,
    );

    let first = plan.peer(
        address,
        plan.connect_timeout(Duration::from_secs(30)),
        plan.server_timeout(Duration::from_secs(30)),
    );
    let second = plan.peer(
        address,
        plan.connect_timeout(Duration::from_secs(30)),
        plan.server_timeout(Duration::from_secs(30)),
    );
    assert_eq!(
        first.options.connection_timeout,
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        first.options.total_connection_timeout,
        Some(Duration::from_secs(2))
    );
    assert_eq!(first.options.read_timeout, Some(Duration::from_secs(11)));
    assert_eq!(first.options.write_timeout, Some(Duration::from_secs(11)));
    assert_eq!(first.options.idle_timeout, Some(Duration::ZERO));
    assert_ne!(first.reuse_hash(), second.reuse_hash());

    let always = UpstreamPlan::with_policy(
        Arc::new(RoundRobinPool::new([address]).expect("always selector")),
        None,
        None,
        None,
        UpstreamConnectionReuse::Always,
    );
    assert_eq!(
        always
            .peer(address, Duration::from_secs(1), Duration::from_secs(1))
            .reuse_hash(),
        always
            .peer(address, Duration::from_secs(1), Duration::from_secs(1))
            .reuse_hash()
    );
}

#[test]
fn h2_only_policy_rejects_downgrade_before_upstream_write() {
    let tls = upstream_tls_plan(
        "origin.example.test",
        HttpVersion::Http2,
        HttpVersion::Http2,
    );

    let error = enforce_http_version(Some(&tls), Version::HTTP_11)
        .expect_err("HTTP/1.1 downgrade must fail");
    assert_eq!(error.etype(), &ErrorType::H2Downgrade);
    assert_eq!(error.esource(), &ErrorSource::Upstream);
    assert!(!error.retry());
    assert!(error.to_string().contains("requires HTTP/2"));
    assert!(enforce_http_version(Some(&tls), Version::HTTP_2).is_ok());
}

#[test]
fn tls_version_policy_requires_a_modern_negotiated_digest() {
    let tls = upstream_tls_plan(
        "origin.example.test",
        HttpVersion::Http11,
        HttpVersion::Http11,
    );

    for digest in [None, Some(&Digest::default())] {
        let error =
            validate_tls_connection(Some(&tls), digest).expect_err("missing TLS digest must fail");
        assert_eq!(error.etype(), &ErrorType::Custom("UpstreamTlsPolicy"));
        assert_eq!(error.esource(), &ErrorSource::Upstream);
        assert!(!error.retry());
    }
    for version in ["TLSv1", "TLSv1.1", "unknown"] {
        let digest = tls_digest(version);
        let error = validate_tls_connection(Some(&tls), Some(&digest))
            .expect_err("obsolete or unknown TLS must fail");
        assert!(error.to_string().contains(version));
        assert!(!error.retry());
    }
    for version in ["TLSv1.2", "TLSv1.3"] {
        let digest = tls_digest(version);
        assert!(validate_tls_connection(Some(&tls), Some(&digest)).is_ok());
    }
    assert!(validate_tls_connection(None, None).is_ok());
}

fn upstream_tls_plan(
    server_name: &str,
    min: HttpVersion,
    max: HttpVersion,
) -> Arc<UpstreamTlsPlan> {
    let pool = UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![UpstreamEndpoint::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], 443)),
        }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: server_name.into(),
            ca_certificate_path: None,
        }),
        http_versions: HttpVersionPolicy { min, max },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        resilience: oxiroute_config::PoolResilience::default(),
    };
    Arc::new(
        crate::tls::prepare_upstream_tls(&pool)
            .expect("upstream TLS plan")
            .expect("TLS enabled"),
    )
}

fn tls_digest(version: &'static str) -> Digest {
    Digest {
        ssl_digest: Some(Arc::new(SslDigest::new(
            "test cipher",
            version,
            None,
            None,
            Vec::new(),
        ))),
        ..Digest::default()
    }
}
