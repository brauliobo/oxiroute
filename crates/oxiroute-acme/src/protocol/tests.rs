use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crate::FakeClock;
use openssl::asn1::Asn1Time;

use super::*;

#[derive(Clone)]
struct ScriptedTransport {
    responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl ScriptedTransport {
    fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl AcmeTransport for ScriptedTransport {
    fn request(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().expect("requests lock").push(request);
        self.responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or(TransportError)
    }
}

fn directory(url: &str) -> HttpResponse {
    HttpResponse::new(
            200,
            url,
            br#"{"newNonce":"https://acme.test/acme/new-nonce","newAccount":"https://acme.test/acme/new-account","newOrder":"https://acme.test/acme/new-order","meta":{"termsOfService":"https://acme.test/terms"}}"#.to_vec(),
        )
}

fn directory_with_actions(url: &str) -> HttpResponse {
    HttpResponse::new(
            200,
            url,
            br#"{"newNonce":"https://acme.test/acme/new-nonce","newAccount":"https://acme.test/acme/new-account","newOrder":"https://acme.test/acme/new-order","revokeCert":"https://acme.test/acme/revoke","keyChange":"https://acme.test/acme/key-change","meta":{"termsOfService":"https://acme.test/terms"}}"#.to_vec(),
        )
}

fn directory_with_renewal_info(url: &str) -> HttpResponse {
    HttpResponse::new(
            200,
            url,
            br#"{"newNonce":"https://acme.test/acme/new-nonce","newAccount":"https://acme.test/acme/new-account","newOrder":"https://acme.test/acme/new-order","renewalInfo":"https://acme.test/acme/renewal-info"}"#.to_vec(),
        )
}

fn account() -> HttpResponse {
    HttpResponse::new(
        201,
        "https://acme.test/acme/new-account",
        br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#
            .to_vec(),
    )
    .with_header("location", "https://acme.test/acme/acct/1")
    .with_header("replay-nonce", "nonce-account")
}

fn order() -> HttpResponse {
    HttpResponse::new(
            201,
            "https://acme.test/acme/new-order",
            br#"{"status":"pending","identifiers":[{"type":"dns","value":"example.test"}],"authorizations":["https://acme.test/acme/authz/1"],"finalize":"https://acme.test/acme/order/1/finalize"}"#.to_vec(),
        )
        .with_header("location", "https://acme.test/acme/order/1")
        .with_header("replay-nonce", "nonce-order")
}

#[test]
fn rejects_private_origins_without_explicit_allowlist() {
    assert!(matches!(
        OriginPolicy::strict("https://127.0.0.1:14000/directory"),
        Err(AcmeError::PrivateOriginRequiresAllowlist)
    ));
    assert!(OriginPolicy::development_allowlist("https://127.0.0.1:14000/directory").is_ok());
}

#[test]
fn classifies_transient_acme_failures_without_retrying_permanent_problems() {
    assert_eq!(
        AcmeError::Problem {
            problem_type: "urn:ietf:params:acme:error:rateLimited".into(),
        }
        .failure_class(),
        AcmeFailureClass::Retryable
    );
    assert_eq!(
        AcmeError::UnexpectedStatus { status: 503 }.failure_class(),
        AcmeFailureClass::Retryable
    );
    assert_eq!(
        AcmeError::Problem {
            problem_type: "urn:ietf:params:acme:error:rejectedIdentifier".into(),
        }
        .failure_class(),
        AcmeFailureClass::Permanent
    );
    assert!(!AcmeError::TermsNotAgreed.is_retryable());
}

#[test]
fn persisted_accounts_must_retain_explicit_terms_agreement() {
    let transport = ScriptedTransport::new([directory("https://acme.test/directory")]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    assert!(matches!(
        client.set_account(Account {
            url: "https://acme.test/acme/acct/1".into(),
            status: "valid".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: false,
        }),
        Err(AcmeError::TermsNotAgreed)
    ));
}

#[test]
fn directory_rejects_cross_origin_redirects_and_advertised_endpoints() {
    let transport = ScriptedTransport::new([HttpResponse::new(
        302,
        "https://evil.test/directory",
        Vec::new(),
    )]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    assert!(matches!(
        Directory::fetch(&transport, "https://acme.test/directory", &policy),
        Err(AcmeError::UntrustedRedirect)
    ));
}

#[test]
fn bad_nonce_is_retried_exactly_once_with_a_fresh_nonce() {
    let responses = [
        directory("https://acme.test/directory"),
        HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
            .with_header("replay-nonce", "nonce-one"),
        HttpResponse::new(
            400,
            "https://acme.test/acme/new-account",
            br#"{"type":"urn:ietf:params:acme:error:badNonce"}"#.to_vec(),
        )
        .with_header("replay-nonce", "nonce-two"),
        account(),
    ];
    let transport = ScriptedTransport::new(responses);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let clock = Arc::new(FakeClock::new(100));
    let mut client = AcmeClient::new(
        transport.clone(),
        "https://acme.test/directory",
        policy,
        key,
        clock,
    )
    .expect("client");
    let account = client
        .register_account(&AccountRequest {
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");
    assert_eq!(account.url, "https://acme.test/acme/acct/1");
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        2
    );
}

#[test]
fn order_state_and_identifiers_are_strict() {
    for (status, expected_error) in [("processing", None), ("unknown", Some("invalid"))] {
        let order = format!(
            "{{\"status\":\"{status}\",\"identifiers\":[{{\"type\":\"dns\",\"value\":\"other.test\"}}],\"authorizations\":[\"https://acme.test/acme/authz/1\"],\"finalize\":\"https://acme.test/acme/order/1/finalize\"}}"
        );
        let transport = ScriptedTransport::new([
            directory("https://acme.test/directory"),
            HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
                .with_header("replay-nonce", "nonce-account"),
            account(),
            HttpResponse::new(201, "https://acme.test/acme/new-order", order.into_bytes())
                .with_header("location", "https://acme.test/acme/order/1")
                .with_header("replay-nonce", "nonce-order"),
        ]);
        let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
        let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
        let mut client = AcmeClient::new(
            transport,
            "https://acme.test/directory",
            policy,
            key,
            Arc::new(FakeClock::new(100)),
        )
        .expect("client");
        client
            .register_account(&AccountRequest {
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
            })
            .expect("account");
        let error = client
            .create_order(&CertificateRequest {
                identifiers: vec!["example.test".into()],
            })
            .expect_err("invalid order");
        if expected_error.is_some() {
            assert!(matches!(error, AcmeError::InvalidOrderState { .. }));
        } else {
            assert!(matches!(error, AcmeError::OrderIdentifiersMismatch));
        }
    }
}

#[test]
fn nonce_is_not_cached_before_response_origin_validation() {
    let transport = ScriptedTransport::new([
        directory("https://acme.test/directory"),
        HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
            .with_header("replay-nonce", "nonce-account"),
        HttpResponse::new(
            400,
            "https://evil.test/acme/new-account",
            br#"{"type":"urn:ietf:params:acme:error:badNonce"}"#.to_vec(),
        )
        .with_header("replay-nonce", "evil-nonce"),
    ]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    assert!(matches!(
        client.register_account(&AccountRequest {
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        }),
        Err(AcmeError::UntrustedRedirect)
    ));
    assert!(client.nonces.is_empty());
}

#[test]
fn account_key_rollover_uses_old_outer_and_new_inner_signatures() {
    let transport = ScriptedTransport::new([
            directory_with_actions("https://acme.test/directory"),
            HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
                .with_header("replay-nonce", "nonce-account"),
            account(),
            HttpResponse::new(
                200,
                "https://acme.test/acme/key-change",
                br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#.to_vec(),
            )
            .with_header("replay-nonce", "nonce-rollover"),
        ]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let old_key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("old key");
    let new_key = AccountKey::generate(AccountKeyAlgorithm::Rsa2048).expect("new key");
    let old_thumbprint = old_key.thumbprint();
    let mut client = AcmeClient::new(
        transport.clone(),
        "https://acme.test/directory",
        policy,
        old_key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    client
        .register_account(&AccountRequest {
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");
    client
        .rollover_account_key(new_key)
        .expect("rollover account");
    assert_ne!(client.key.thumbprint(), old_thumbprint);

    let requests = transport.requests();
    let envelope: Value = serde_json::from_slice(&requests[3].body).expect("outer JWS");
    let outer_protected = decode_jws_protected(&envelope);
    assert!(outer_protected.get("kid").is_some());
    let inner_payload = envelope
        .get("payload")
        .and_then(Value::as_str)
        .expect("inner payload");
    let inner: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(inner_payload)
            .expect("inner JWS encoding"),
    )
    .expect("inner JWS");
    let inner_protected = decode_jws_protected(&inner);
    assert!(inner_protected.get("jwk").is_some());
    assert!(inner.get("signature").and_then(Value::as_str).is_some());
}

#[test]
fn account_key_rollover_rebuilds_nested_jws_for_one_bad_nonce_retry() {
    let transport = ScriptedTransport::new([
            directory_with_actions("https://acme.test/directory"),
            HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
                .with_header("replay-nonce", "nonce-registration"),
            account(),
            HttpResponse::new(
                400,
                "https://acme.test/acme/key-change",
                br#"{"type":"urn:ietf:params:acme:error:badNonce"}"#.to_vec(),
            )
            .with_header("replay-nonce", "nonce-rollover-retry"),
            HttpResponse::new(
                200,
                "https://acme.test/acme/key-change",
                br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#.to_vec(),
            ),
        ]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let old_key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("old key");
    let new_key = AccountKey::generate(AccountKeyAlgorithm::Rsa2048).expect("new key");
    let mut client = AcmeClient::new(
        transport.clone(),
        "https://acme.test/directory",
        policy,
        old_key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    client
        .register_account(&AccountRequest {
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");

    client
        .rollover_account_key(new_key)
        .expect("retried rollover");

    let requests = transport.requests();
    let attempts = [&requests[3], &requests[4]];
    let mut nonces = Vec::new();
    for request in attempts {
        let outer: Value = serde_json::from_slice(&request.body).expect("outer JWS");
        let protected = decode_jws_protected(&outer);
        assert!(protected.get("kid").is_some());
        nonces.push(
            protected
                .get("nonce")
                .and_then(Value::as_str)
                .expect("outer nonce")
                .to_owned(),
        );
        let inner: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(
                    outer
                        .get("payload")
                        .and_then(Value::as_str)
                        .expect("inner payload"),
                )
                .expect("inner encoding"),
        )
        .expect("inner JWS");
        assert!(decode_jws_protected(&inner).get("jwk").is_some());
    }
    assert_eq!(nonces, ["nonce-account", "nonce-rollover-retry"]);
}

#[test]
fn revocation_rejects_unknown_reason_codes() {
    let transport = ScriptedTransport::new([directory_with_actions("https://acme.test/directory")]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    assert!(matches!(
        client.revoke_certificate(b"not a certificate", Some(9)),
        Err(AcmeError::InvalidRevocationReason)
    ));
}

#[test]
fn polling_cancellation_stops_before_the_next_request() {
    let transport = ScriptedTransport::new([directory("https://acme.test/directory")]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport.clone(),
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    client
        .set_account(Account {
            url: "https://acme.test/acme/acct/1".into(),
            status: "valid".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");
    let cancellation = Dns01Cancellation::new();
    cancellation.cancel();
    assert!(matches!(
        client.poll_order(
            "https://acme.test/acme/order/1",
            &PollPolicy {
                cancellation: Some(cancellation),
                ..PollPolicy::default()
            }
        ),
        Err(AcmeError::Cancelled)
    ));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn polling_cancellation_after_a_pending_response_prevents_sleep() {
    let clock = FakeClock::new(100);
    let cancellation = Dns01Cancellation::new();
    let mut attempts = 0;
    let result = poll_acme(
        &clock,
        "https://acme.test/acme/order/1",
        &PollPolicy {
            max_attempts: 2,
            deadline_unix_seconds: 200,
            initial_delay_seconds: 10,
            max_delay_seconds: 10,
            cancellation: Some(cancellation.clone()),
        },
        || {
            attempts += 1;
            cancellation.cancel();
            Ok::<_, AcmeError>(PollAttempt::<()>::Pending(HttpResponse::new(
                200,
                "https://acme.test/acme/order/1",
                Vec::new(),
            )))
        },
    );

    assert!(matches!(result, Err(AcmeError::Cancelled)));
    assert_eq!(attempts, 1);
    assert_eq!(clock.now_unix_seconds(), 100);
}

struct BlockingClock {
    reached: Arc<std::sync::Barrier>,
}

impl Clock for BlockingClock {
    fn now_unix_seconds(&self) -> u64 {
        100
    }

    fn sleep_seconds_cancellable(&self, seconds: u64, cancellation: &Dns01Cancellation) -> bool {
        self.reached.wait();
        cancellation.wait_timeout(std::time::Duration::from_secs(seconds))
    }
}

#[test]
fn polling_cancellation_interrupts_an_active_sleep() {
    let clock = Arc::new(BlockingClock {
        reached: Arc::new(std::sync::Barrier::new(2)),
    });
    let cancellation = Dns01Cancellation::new();
    let worker_clock = Arc::clone(&clock);
    let worker_cancellation = cancellation.clone();
    let worker = std::thread::spawn(move || {
        poll_acme(
            worker_clock.as_ref(),
            "https://acme.test/acme/order/1",
            &PollPolicy {
                max_attempts: 2,
                deadline_unix_seconds: 200,
                initial_delay_seconds: 60,
                max_delay_seconds: 60,
                cancellation: Some(worker_cancellation),
            },
            || {
                Ok::<_, AcmeError>(PollAttempt::<()>::Pending(HttpResponse::new(
                    200,
                    "https://acme.test/acme/order/1",
                    Vec::new(),
                )))
            },
        )
    });
    clock.reached.wait();
    let started = std::time::Instant::now();

    cancellation.cancel();

    assert!(matches!(worker.join(), Ok(Err(AcmeError::Cancelled))));
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
}

#[test]
fn polling_cancellation_bound_is_stable_under_parallel_load() {
    const POLLERS: usize = 64;

    let entered = Arc::new(std::sync::Barrier::new(POLLERS + 1));
    let cancellations = (0..POLLERS)
        .map(|_| Dns01Cancellation::new())
        .collect::<Vec<_>>();
    let workers = cancellations
        .iter()
        .cloned()
        .map(|cancellation| {
            let entered = Arc::clone(&entered);
            std::thread::spawn(move || {
                let clock = BlockingClock { reached: entered };
                poll_acme(
                    &clock,
                    "https://acme.test/acme/order/1",
                    &PollPolicy {
                        max_attempts: 2,
                        deadline_unix_seconds: 200,
                        initial_delay_seconds: 60,
                        max_delay_seconds: 60,
                        cancellation: Some(cancellation),
                    },
                    || {
                        Ok::<_, AcmeError>(PollAttempt::<()>::Pending(HttpResponse::new(
                            200,
                            "https://acme.test/acme/order/1",
                            Vec::new(),
                        )))
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    entered.wait();
    let started = std::time::Instant::now();

    for cancellation in &cancellations {
        cancellation.cancel();
    }
    for worker in workers {
        assert!(matches!(worker.join(), Ok(Err(AcmeError::Cancelled))));
    }

    assert!(started.elapsed() < std::time::Duration::from_millis(500));
}

#[test]
fn renewal_information_uses_the_leaf_hash_and_bounded_window() {
    let certificate = renewal_test_certificate();
    let transport = ScriptedTransport::new([
        directory_with_renewal_info("https://acme.test/directory"),
        HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
            .with_header("replay-nonce", "nonce-ari"),
        HttpResponse::new(
            200,
            "https://acme.test/acme/renewal-info",
            br#"{"suggestedWindow":{"start":"2026-08-10T00:00:00Z","end":"2026-08-20T00:00:00Z"}}"#
                .to_vec(),
        )
        .with_header("replay-nonce", "nonce-ari-response"),
    ]);
    let requests = transport.clone();
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    client
        .set_account(Account {
            url: "https://acme.test/acme/acct/1".into(),
            status: "valid".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");

    let information = client
        .renewal_information(&certificate)
        .expect("renewal information")
        .expect("advertised renewal information");
    assert!(
        information.suggested_window_start_unix_seconds
            < information.suggested_window_end_unix_seconds
    );
    let requested = requests.requests();
    assert!(
        requested[2]
            .url
            .starts_with("https://acme.test/acme/renewal-info/")
    );
    assert_eq!(requested[2].method, "POST");
}

#[test]
fn renewal_information_rejects_an_unbounded_window() {
    let certificate = renewal_test_certificate();
    let transport = ScriptedTransport::new([
        directory_with_renewal_info("https://acme.test/directory"),
        HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
            .with_header("replay-nonce", "nonce-ari"),
        HttpResponse::new(
            200,
            "https://acme.test/acme/renewal-info",
            br#"{"suggestedWindow":{"start":"2026-01-01T00:00:00Z","end":"2028-01-01T00:00:00Z"}}"#
                .to_vec(),
        ),
    ]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    client
        .set_account(Account {
            url: "https://acme.test/acme/acct/1".into(),
            status: "valid".into(),
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");
    assert!(matches!(
        client.renewal_information(&certificate),
        Err(AcmeError::InvalidRenewalInformation)
    ));
}

#[test]
fn ip_identifiers_are_explicitly_unsupported_for_orders_and_csrs() {
    let transport = ScriptedTransport::new([directory("https://acme.test/directory")]);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let mut client = AcmeClient::new(
        transport,
        "https://acme.test/directory",
        policy,
        key,
        Arc::new(FakeClock::new(100)),
    )
    .expect("client");
    assert!(matches!(
        client.create_order(&CertificateRequest {
            identifiers: vec!["192.0.2.10".into()],
        }),
        Err(AcmeError::IpIdentifierUnsupported)
    ));
    assert!(matches!(
        generate_leaf_csr(&["2001:db8::1".into()], LeafKeyAlgorithm::EcdsaP256),
        Err(AcmeError::IpIdentifierUnsupported)
    ));
}

fn renewal_test_certificate() -> Vec<u8> {
    let key = PKey::from_rsa(Rsa::generate(2048).expect("key")).expect("PKey");
    let mut name = X509NameBuilder::new().expect("name");
    name.append_entry_by_text("commonName", "ari.example.test")
        .expect("common name");
    let name = name.build();
    let mut builder = X509::builder().expect("builder");
    builder.set_version(2).expect("version");
    builder.set_subject_name(&name).expect("subject");
    builder.set_issuer_name(&name).expect("issuer");
    builder.set_pubkey(&key).expect("public key");
    builder
        .set_not_before(&Asn1Time::days_from_now(0).expect("not before"))
        .expect("not before");
    builder
        .set_not_after(&Asn1Time::days_from_now(30).expect("not after"))
        .expect("not after");
    builder
        .sign(&key, MessageDigest::sha256())
        .expect("signature");
    builder.build().to_pem().expect("certificate")
}

fn decode_jws_protected(envelope: &Value) -> Value {
    let protected = envelope
        .get("protected")
        .and_then(Value::as_str)
        .expect("protected header");
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(protected)
            .expect("protected encoding"),
    )
    .expect("protected JSON")
}

#[test]
fn polling_bounds_cap_delay_and_accept_http_date_retry_guidance() {
    let (attempts, initial, maximum) = bounded_poll_policy(&PollPolicy {
        max_attempts: usize::MAX,
        deadline_unix_seconds: u64::MAX,
        initial_delay_seconds: u64::MAX,
        max_delay_seconds: u64::MAX,
        cancellation: None,
    });
    assert_eq!(attempts, MAX_POLL_ATTEMPTS);
    assert_eq!(initial, MAX_POLL_DELAY_SECONDS);
    assert_eq!(maximum, MAX_POLL_DELAY_SECONDS);
    assert_eq!(jittered_delay(10, 10, "authz", 1), 10);
    let response = HttpResponse::new(200, "https://acme.test/authz", Vec::new())
        .with_header("retry-after", "Thu, 01 Jan 1970 00:02:00 GMT");
    assert_eq!(retry_after(&response, 60).expect("date"), Some(60));
}

#[test]
fn full_http01_order_path_is_bounded_and_exact() {
    let responses = [
            directory("https://acme.test/directory"),
            HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
                .with_header("replay-nonce", "nonce-account"),
            account(),
            order(),
            HttpResponse::new(
                200,
                "https://acme.test/acme/authz/1",
                br#"{"status":"pending","identifier":{"type":"dns","value":"example.test"},"challenges":[{"type":"http-01","url":"https://acme.test/acme/challenge/1","token":"abc_DEF-123"}]}"#.to_vec(),
            )
            .with_header("replay-nonce", "nonce-authz"),
            HttpResponse::new(200, "https://acme.test/acme/challenge/1", br#"{"status":"processing"}"#.to_vec())
                .with_header("replay-nonce", "nonce-respond"),
            HttpResponse::new(
                200,
                "https://acme.test/acme/authz/1",
                br#"{"status":"valid","identifier":{"type":"dns","value":"example.test"},"challenges":[]}"#.to_vec(),
            )
            .with_header("replay-nonce", "nonce-valid"),
        ];
    let transport = ScriptedTransport::new(responses);
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let clock = Arc::new(FakeClock::new(100));
    let mut client = AcmeClient::new(transport, "https://acme.test/directory", policy, key, clock)
        .expect("client");
    client
        .register_account(&AccountRequest {
            contacts: vec!["mailto:ops@example.test".into()],
            terms_agreed: true,
        })
        .expect("account");
    let order = client
        .create_order(&CertificateRequest {
            identifiers: vec!["EXAMPLE.TEST".into()],
        })
        .expect("order");
    let challenge = client
        .authorization(&order.authorizations[0])
        .expect("challenge")
        .challenge
        .expect("HTTP-01");
    assert_eq!(challenge.key_authorization.split('.').count(), 2);
    client.respond_to_challenge(&challenge).expect("respond");
    let authorization = client
        .poll_authorization(
            &challenge.authorization_url,
            &PollPolicy {
                max_attempts: 2,
                deadline_unix_seconds: 110,
                initial_delay_seconds: 1,
                max_delay_seconds: 1,
                cancellation: None,
            },
        )
        .expect("valid authorization");
    assert_eq!(authorization.status, AuthorizationStatus::Valid);
}

#[test]
fn tls_alpn01_authorization_selects_the_exact_challenge_and_key_authorization() {
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let response = HttpResponse::new(
            200,
            "https://acme.test/acme/authz/1",
            br#"{"status":"pending","identifier":{"type":"dns","value":"EXAMPLE.TEST"},"challenges":[{"type":"tls-alpn-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]}"#.to_vec(),
        );
    let authorization = parse_authorization(
        &response,
        "https://acme.test/acme/authz/1",
        &key,
        &policy,
        ChallengeType::TlsAlpn01,
    )
    .expect("TLS-ALPN-01 authorization");
    let challenge = authorization
        .tls_alpn01_challenge
        .expect("TLS-ALPN-01 challenge");
    assert_eq!(challenge.token, "token-1");
    assert_eq!(
        challenge.key_authorization,
        key.key_authorization("token-1")
    );
    assert!(authorization.challenge.is_none());
    assert!(authorization.dns01_challenge.is_none());
}

#[test]
fn dns01_authorization_derives_the_wildcard_txt_record() {
    let key = AccountKey::generate(AccountKeyAlgorithm::EcdsaP256).expect("key");
    let policy = OriginPolicy::strict("https://acme.test/directory").expect("policy");
    let response = HttpResponse::new(
            200,
            "https://acme.test/acme/authz/1",
            br#"{"status":"pending","identifier":{"type":"dns","value":"*.EXAMPLE.TEST"},"challenges":[{"type":"dns-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]}"#.to_vec(),
        );
    let authorization = parse_authorization(
        &response,
        "https://acme.test/acme/authz/1",
        &key,
        &policy,
        ChallengeType::Dns01,
    )
    .expect("DNS-01 authorization");
    let challenge = authorization.dns01_challenge.expect("DNS-01 challenge");
    let expected_value =
        URL_SAFE_NO_PAD.encode(Sha256::digest(key.key_authorization("token-1").as_bytes()));
    assert_eq!(challenge.identifier(), "*.example.test");
    assert_eq!(challenge.record_name(), "_acme-challenge.example.test");
    assert_eq!(challenge.record_value(), expected_value);
}

#[test]
fn csr_contains_exact_sans_and_private_debug_is_redacted() {
    let csr = generate_leaf_csr(
        &["example.test".into(), "www.example.test".into()],
        LeafKeyAlgorithm::EcdsaP256,
    )
    .expect("CSR");
    let request = X509Req::from_der(&csr.csr_der).expect("DER CSR");
    let public = request.public_key().expect("public key");
    assert!(request.verify(&public).expect("verify CSR"));
    assert!(format!("{csr:?}").contains("REDACTED"));
    assert!(!format!("{csr:?}").contains("BEGIN PRIVATE KEY"));
}
