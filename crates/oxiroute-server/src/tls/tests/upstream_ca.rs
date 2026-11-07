#[allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn compiles_custom_ca_peer_policy_and_isolates_reuse() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "upstream", "origin.example.test", false);
    let custom_pool = upstream_pool(
        Some(files.ca.clone()),
        HttpVersion::Http11,
        HttpVersion::Http2,
    );
    let custom = prepare_upstream_tls(&custom_pool).unwrap().unwrap();
    let _: &UpstreamTlsPlan = &custom;
    let same = prepare_upstream_tls(&custom_pool).unwrap().unwrap();
    assert_eq!(custom.server_name(), "origin.example.test");
    assert_eq!(custom.min_http_version(), HttpVersion::Http11);
    assert_eq!(custom.max_http_version(), HttpVersion::Http2);
    assert!(custom.uses_custom_ca());
    assert_ne!(custom.group_key(), 0);
    assert_eq!(custom.group_key(), same.group_key());

    let mut peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    peer.options.verify_cert = false;
    peer.options.verify_hostname = false;
    custom.apply_to_peer(&mut peer);
    assert!(peer.is_tls());
    assert_eq!(peer.sni, "origin.example.test");
    assert!(peer.options.verify_cert);
    assert!(peer.options.verify_hostname);
    assert!(peer.options.ca.is_some());
    assert!(peer.options.upstream_tls_configure_hook.is_some());
    assert_eq!(peer.options.alpn, ALPN::H2H1);
    assert_eq!(peer.group_key, custom.group_key());

    let mut same_peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    same.apply_to_peer(&mut same_peer);
    assert!(Arc::ptr_eq(
        peer.options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("custom plan TLS configure hook"),
        same_peer
            .options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("same plan TLS configure hook"),
    ));

    let system_pool = upstream_pool(None, HttpVersion::Http11, HttpVersion::Http2);
    let system = prepare_upstream_tls(&system_pool).unwrap().unwrap();
    assert!(!system.uses_custom_ca());
    assert_ne!(custom.group_key(), system.group_key());
    let mut system_peer = HttpPeer::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 443)),
        false,
        String::new(),
    );
    system.apply_to_peer(&mut system_peer);
    assert!(system_peer.options.ca.is_none());
    assert!(!Arc::ptr_eq(
        peer.options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("custom CA verification hook"),
        system_peer
            .options
            .upstream_tls_configure_hook
            .as_ref()
            .expect("system root verification hook"),
    ));

    let h2_pool = upstream_pool(Some(files.ca), HttpVersion::Http2, HttpVersion::Http2);
    let h2 = prepare_upstream_tls(&h2_pool).unwrap().unwrap();
    assert_ne!(custom.group_key(), h2.group_key());
}

#[test]
fn rejects_malformed_custom_ca() {
    let temp = TempDir::new().unwrap();
    let malformed = temp.path().join("bad-ca.pem");
    fs::write(
        &malformed,
        b"-----BEGIN CERTIFICATE-----\nnot-a-certificate\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let pool = upstream_pool(Some(malformed), HttpVersion::Http11, HttpVersion::Http11);
    let error = prepare_upstream_tls(&pool).unwrap_err();
    assert!(matches!(error, TlsBuildError::CaParse { .. }));
    assert!(error.source().is_some());
}

#[test]
fn rejects_empty_duplicate_expired_and_non_ca_custom_anchors() {
    let temp = TempDir::new().unwrap();
    let files = write_identity(temp.path(), "anchors", "anchor.example.test", false);

    let empty = temp.path().join("empty-ca.pem");
    fs::write(&empty, []).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(empty),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(error, TlsBuildError::EmptyFile { .. }));

    let duplicate = temp.path().join("duplicate-ca.pem");
    let ca_pem = fs::read(&files.ca).unwrap();
    let mut duplicate_pem = ca_pem.clone();
    duplicate_pem.extend_from_slice(&ca_pem);
    fs::write(&duplicate, duplicate_pem).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(duplicate),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::DuplicateCaCertificate { index: 1, .. }
    ));

    let expired_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let expired_ca = build_certificate(
        "Expired Root",
        None,
        &expired_key,
        &expired_key,
        &[],
        true,
        true,
        false,
    );
    let expired_path = temp.path().join("expired-ca.pem");
    fs::write(&expired_path, expired_ca.to_pem().unwrap()).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(expired_path),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::CaCertificateExpired { index: 0, .. }
    ));

    let certificates = X509::stack_from_pem(&fs::read(&files.chain).unwrap()).unwrap();
    let non_ca_path = temp.path().join("non-ca-anchor.pem");
    fs::write(&non_ca_path, certificates[0].to_pem().unwrap()).unwrap();
    let error = prepare_upstream_tls(&upstream_pool(
        Some(non_ca_path),
        HttpVersion::Http11,
        HttpVersion::Http11,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        TlsBuildError::NonCaCertificate { index: 0, .. }
    ));
}
