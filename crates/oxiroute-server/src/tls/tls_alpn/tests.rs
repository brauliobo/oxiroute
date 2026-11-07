use super::*;

fn challenge(
    identifier: &str,
    created_at_unix_seconds: u64,
    expires_at_unix_seconds: u64,
) -> TlsAlpnChallenge {
    TlsAlpnChallenge::generate(
        identifier,
        "token.thumbprint",
        "account-1",
        "order-1",
        "authorization-1",
        "challenge-1",
        created_at_unix_seconds,
        expires_at_unix_seconds,
    )
    .expect("TLS-ALPN-01 challenge")
}

#[test]
fn generates_exact_rfc8737_certificate_and_redacts_private_material() {
    let challenge = challenge("WWW.EXAMPLE.TEST", 1, 100);
    let debug = format!("{challenge:?}");
    assert!(debug.contains("www.example.test"));
    assert!(!debug.contains("token.thumbprint"));
    assert!(debug.contains("REDACTED"));
    let certificate = challenge.identity.certificate();
    assert_eq!(certificate.subject_alt_names().unwrap().len(), 1);
    assert_eq!(
        certificate
            .subject_alt_names()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .dnsname(),
        Some("www.example.test")
    );
    let der = certificate.to_der().expect("challenge certificate DER");
    let (_, parsed) = parse_x509_certificate(&der).expect("parse challenge certificate");
    let extension = parsed
        .extensions()
        .iter()
        .find(|extension| extension.oid.to_id_string() == TLS_ALPN_IDENTIFIER_OID)
        .expect("ACME identifier extension");
    let mut expected_value = vec![0x04, 0x20];
    expected_value.extend_from_slice(&openssl::sha::sha256(b"token.thumbprint"));
    assert!(extension.critical);
    assert_eq!(extension.value, expected_value.as_slice());
}

#[test]
fn store_fences_expiry_cancellation_and_concurrent_ownership() {
    let store = TlsAlpnChallengeStore::new(1);
    let first = store
        .provision(challenge("one.example.test", 1, 100))
        .unwrap();
    assert!(matches!(
        store.provision(challenge("one.example.test", 2, 100)),
        Err(TlsAlpnChallengeError::DuplicateIdentifier)
    ));
    assert!(store.lookup("one.example.test", 99).is_some());
    first.cancel();
    assert!(store.lookup("one.example.test", 2).is_none());

    let expired = store
        .provision(challenge("expired.example.test", 1, 2))
        .unwrap();
    assert!(store.lookup("expired.example.test", 2).is_none());
    drop(expired);
    assert!(store.is_empty());
}

#[test]
fn rejects_wildcard_ip_and_invalid_lifetime_without_partial_state() {
    assert!(matches!(
        TlsAlpnChallenge::generate(
            "*.example.test",
            "token.thumbprint",
            "account-1",
            "order-1",
            "authorization-1",
            "challenge-1",
            1,
            2,
        ),
        Err(TlsAlpnChallengeError::InvalidIdentifier)
    ));
    assert!(matches!(
        TlsAlpnChallenge::generate(
            "127.0.0.1",
            "token.thumbprint",
            "account-1",
            "order-1",
            "authorization-1",
            "challenge-1",
            1,
            2,
        ),
        Err(TlsAlpnChallengeError::InvalidIdentifier)
    ));
    assert!(matches!(
        TlsAlpnChallenge::generate(
            "example.test",
            "token.thumbprint",
            "account-1",
            "order-1",
            "authorization-1",
            "challenge-1",
            2,
            1,
        ),
        Err(TlsAlpnChallengeError::InvalidLifetime)
    ));
}
