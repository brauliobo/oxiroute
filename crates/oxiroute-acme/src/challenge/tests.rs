use super::*;

fn record(token: &str, expires_at_unix_seconds: u64) -> ChallengeRecord {
    ChallengeRecord {
        token: token.into(),
        key_authorization: format!("{token}.thumbprint"),
        account_id: "acct-1".into(),
        order_id: "order-1".into(),
        authorization_id: "authz-1".into(),
        challenge_id: "challenge-1".into(),
        created_at_unix_seconds: 1,
        expires_at_unix_seconds,
    }
}

#[test]
fn exact_token_route_wins_and_unknown_values_are_empty_not_found() {
    let store = ChallengeStore::default();
    let lease = store
        .provision(record("abc_DEF-1", 100))
        .expect("provision");
    assert_eq!(
        store
            .route("GET", "/.well-known/acme-challenge/abc_DEF-1", 50)
            .expect("challenge route")
            .status,
        200
    );
    assert_eq!(
        store
            .route("GET", "/.well-known/acme-challenge/unknown", 50)
            .expect("unknown challenge route"),
        ChallengeHttpResponse::not_found()
    );
    assert_eq!(
        store
            .route("GET", "/.well-known/acme-challenge/abc_DEF-1/extra", 50)
            .expect("malformed challenge route"),
        ChallengeHttpResponse::not_found()
    );
    assert!(store.route("GET", "/normal", 50).is_none());
    lease.complete();
    assert!(store.is_empty());
}

#[test]
fn head_returns_no_secret_body_but_get_returns_only_key_authorization() {
    let store = ChallengeStore::default();
    let _lease = store.provision(record("token", 100)).expect("provision");
    let head = store
        .route("HEAD", "/.well-known/acme-challenge/token", 50)
        .expect("HEAD");
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    let get = store
        .route("GET", "/.well-known/acme-challenge/token", 50)
        .expect("GET");
    assert_eq!(get.body, b"token.thumbprint");
    assert!(!format!("{store:?}").contains("thumbprint"));
}

#[test]
fn expiry_and_cancellation_are_idempotent_and_bounded() {
    let store = ChallengeStore::new(1);
    let lease = store.provision(record("one", 10)).expect("provision");
    assert!(matches!(
        store.provision(record("two", 20)),
        Err(ChallengeStoreError::CapacityExceeded)
    ));
    assert_eq!(store.reap_expired(10), 1);
    assert_eq!(store.reap_expired(11), 0);
    drop(lease);
    assert!(store.is_empty());
}

#[test]
fn expired_capacity_is_reclaimed_and_old_leases_cannot_cancel_replacements() {
    let store = ChallengeStore::new(1);
    let old = store.provision(record("token", 10)).expect("old challenge");
    assert!(store.provision(record("replacement", 20)).is_err());
    let replacement = store
        .provision(record("replacement", 20))
        .expect_err("capacity remains occupied before expiry");
    assert_eq!(replacement, ChallengeStoreError::CapacityExceeded);
    assert_eq!(store.reap_expired(10), 1);
    let replacement = store
        .provision(record("token", 30))
        .expect("replacement challenge");
    old.cancel();
    assert_eq!(
        store.lookup("token", 11).as_deref(),
        Some("token.thumbprint")
    );
    replacement.cancel();
    assert!(store.is_empty());
}

#[test]
fn challenge_debug_redacts_token_and_key_authorization() {
    let record = record("secret-token", 100);
    let debug = format!("{record:?}");
    assert!(!debug.contains("secret-token"));
    assert!(!debug.contains("secret-token.thumbprint"));
    assert!(debug.contains("REDACTED"));
}

#[test]
fn cleanup_can_target_authorization_without_revealing_other_records() {
    let store = ChallengeStore::default();
    let first = store.provision(record("first", 100)).expect("first");
    let mut second_record = record("second", 100);
    second_record.authorization_id = "authz-2".into();
    let second = store.provision(second_record).expect("second");
    assert_eq!(store.cancel_authorization("authz-1"), 1);
    assert!(store.lookup("first", 50).is_none());
    assert_eq!(
        store.lookup("second", 50).as_deref(),
        Some("second.thumbprint")
    );
    first.cancel();
    second.cancel();
}

#[test]
fn serves_exactly_the_paths_route_handles() {
    let store = ChallengeStore::default();
    for path in [
        "/.well-known/acme-challenge/",
        "/.well-known/acme-challenge/a/b",
        "/.well-known/acme-challenge/x",
    ] {
        assert!(ChallengeStore::serves(path));
        assert!(store.route("GET", path, 0).is_some());
    }
    for path in ["/", "/.well-known/acme-challenge", "/.well-known/other/x"] {
        assert!(!ChallengeStore::serves(path));
        assert!(store.route("GET", path, 0).is_none());
    }
}
