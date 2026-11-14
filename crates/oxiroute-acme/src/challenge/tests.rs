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
fn exact_token_route_wins_and_every_other_request_is_left_to_routing() {
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
    for path in [
        "/.well-known/acme-challenge/unknown",
        "/.well-known/acme-challenge/abc_DEF-1/extra",
        "/.well-known/acme-challenge/abc_DEF-1?x",
        "/.well-known/acme-challenge/",
        "/normal",
    ] {
        assert!(store.route("GET", path, 50).is_none(), "{path}");
    }
    lease.complete();
    assert!(store.is_empty());
}

#[test]
fn only_get_and_head_are_answered_for_a_held_token() {
    let store = ChallengeStore::default();
    let _lease = store.provision(record("token", 100)).expect("provision");
    for method in ["POST", "PUT", "DELETE", "OPTIONS", "get"] {
        assert!(
            store
                .route(method, "/.well-known/acme-challenge/token", 50)
                .is_none(),
            "{method}"
        );
    }
}

#[test]
fn an_expired_token_is_left_to_routing() {
    let store = ChallengeStore::default();
    let _lease = store.provision(record("token", 100)).expect("provision");
    assert!(
        store
            .route("GET", "/.well-known/acme-challenge/token", 100)
            .is_none()
    );
    assert!(!store.holds("/.well-known/acme-challenge/token"));
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
fn holds_exactly_the_paths_of_provisioned_tokens() {
    let store = ChallengeStore::default();
    let lease = store.provision(record("x", 100)).expect("provision");
    assert!(store.holds("/.well-known/acme-challenge/x"));
    for path in [
        "/",
        "/.well-known/acme-challenge",
        "/.well-known/acme-challenge/",
        "/.well-known/acme-challenge/y",
        "/.well-known/acme-challenge/x/y",
        "/.well-known/other/x",
    ] {
        assert!(!store.holds(path), "{path}");
    }
    lease.complete();
    assert!(!store.holds("/.well-known/acme-challenge/x"));
}
