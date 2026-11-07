use http::{Method, Request, header};

use super::{
    ConnectUdpRequestKind, ForwardAccessMetrics, ForwardAccessResult, classify_connect_udp_request,
    source_cidrs_match,
};

#[test]
fn missing_inet_peer_never_matches_source_cidrs() {
    assert!(!source_cidrs_match(&["0.0.0.0/0".into()], None));
}

#[test]
fn ipv4_mapped_peer_matches_ipv4_source_cidr() {
    let client_addr = "[::ffff:127.0.0.1]:54321"
        .parse()
        .expect("mapped loopback address");

    assert!(source_cidrs_match(
        &["127.0.0.0/8".into()],
        Some(client_addr)
    ));
}

#[test]
fn access_metrics_classify_results_without_retaining_request_data() {
    let metrics = ForwardAccessMetrics::default();
    metrics.record(ForwardAccessResult::Allowed);
    metrics.record(ForwardAccessResult::Forbidden);
    metrics.record(ForwardAccessResult::Forbidden);

    assert_eq!(
        metrics.snapshot(),
        super::ForwardAccessMetricsSnapshot {
            allowed: 1,
            forbidden: 2,
            ..super::ForwardAccessMetricsSnapshot::default()
        }
    );
}

#[test]
fn connect_udp_accepts_true_capsule_protocol_parameters() {
    for value in [
        r#"?1;grease="parameter""#,
        "?1;grease",
        "?1;grease=:YWJj:",
        "?1;grease=1.25",
    ] {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/.well-known/masque/udp/example.com/443/")
            .header(header::HOST, "proxy.example.test")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "connect-udp")
            .header("capsule-protocol", value)
            .body(())
            .expect("CONNECT-UDP request");
        assert_eq!(
            classify_connect_udp_request(&request),
            ConnectUdpRequestKind::Valid,
            "rejected {value}"
        );
    }
}

#[test]
fn connect_udp_rejects_framing_and_malformed_masque_attempts() {
    let framed = Request::builder()
        .method(Method::GET)
        .uri("/.well-known/masque/udp/example.com/443/")
        .header(header::HOST, "proxy.example.test")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "connect-udp")
        .header(header::CONTENT_LENGTH, "0")
        .body(())
        .expect("framed CONNECT-UDP request");
    assert_eq!(
        classify_connect_udp_request(&framed),
        ConnectUdpRequestKind::Malformed
    );

    let malformed_path = Request::builder()
        .method(Method::GET)
        .uri("http://origin.example/.well-known/masque/udp/example.com/443")
        .header(header::HOST, "proxy.example.test")
        .header(header::CONNECTION, "close")
        .body(())
        .expect("malformed MASQUE request");
    assert_eq!(
        classify_connect_udp_request(&malformed_path),
        ConnectUdpRequestKind::Malformed
    );

    let ordinary = Request::builder()
        .method(Method::GET)
        .uri("http://origin.example/ordinary")
        .body(())
        .expect("ordinary absolute-form request");
    assert_eq!(
        classify_connect_udp_request(&ordinary),
        ConnectUdpRequestKind::NotAttempt
    );
}
