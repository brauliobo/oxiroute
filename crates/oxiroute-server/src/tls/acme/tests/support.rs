#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct TestCa {
    certificate: X509,
    key: PKey<Private>,
}

#[derive(Clone)]
pub(super) struct FakePebbleTransport {
    pub(super) responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    pub(super) requests: Arc<Mutex<Vec<String>>>,
    pub(super) ca: Arc<TestCa>,
    pub(super) certificate: Arc<Mutex<Option<Vec<u8>>>>,
    pub(super) certificate_names: Vec<String>,
}

impl AcmeTransport for FakePebbleTransport {
    fn request(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests
            .lock()
            .expect("request log")
            .push(request.url.clone());
        if request.url.ends_with("/finalize") {
            let envelope: Value =
                serde_json::from_slice(&request.body).map_err(|_| TransportError)?;
            let payload = envelope
                .get("payload")
                .and_then(Value::as_str)
                .ok_or(TransportError)?;
            let payload = URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| TransportError)?;
            let payload: Value = serde_json::from_slice(&payload).map_err(|_| TransportError)?;
            let csr = payload
                .get("csr")
                .and_then(Value::as_str)
                .ok_or(TransportError)?;
            let csr = URL_SAFE_NO_PAD.decode(csr).map_err(|_| TransportError)?;
            let csr = X509Req::from_der(&csr).map_err(|_| TransportError)?;
            let certificate = issue_certificate(&csr, &self.ca, &self.certificate_names)
                .map_err(|_| TransportError)?;
            *self.certificate.lock().expect("certificate") = Some(certificate);
        }
        if request.url.ends_with("/certificate/1") {
            let body = self
                .certificate
                .lock()
                .expect("certificate")
                .clone()
                .ok_or(TransportError)?;
            return Ok(HttpResponse::new(200, request.url, body));
        }
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .ok_or(TransportError)
    }
}

#[derive(Clone, Copy)]
pub(super) enum BlockingBoundary {
    Connect,
    Write,
    Read,
    Poll,
}

pub(super) struct BlockingTransport {
    pub(super) boundary: BlockingBoundary,
    pub(super) reached: Arc<Barrier>,
    pub(super) cleanup: Arc<AtomicUsize>,
}

impl Drop for BlockingTransport {
    fn drop(&mut self) {
        self.cleanup.fetch_add(1, Ordering::AcqRel);
    }
}

impl AcmeTransport for BlockingTransport {
    fn request(&self, _request: HttpRequest) -> Result<HttpResponse, TransportError> {
        Err(TransportError)
    }

    fn request_with_operation(
        &self,
        _request: HttpRequest,
        operation: &AcmeOperation,
    ) -> Result<HttpResponse, AcmeError> {
        let _ = self.boundary;
        self.reached.wait();
        let _ = operation
            .cancellation()
            .wait_timeout(Duration::from_secs(1));
        operation.check()?;
        Err(AcmeError::Transport(TransportError))
    }
}

#[derive(Default)]
pub(super) struct FakeDnsProvider {
    pub(super) created: Arc<Mutex<Vec<String>>>,
    pub(super) propagated: Arc<Mutex<Vec<String>>>,
    pub(super) cleaned: Arc<Mutex<Vec<String>>>,
    pub(super) cleanup_fail: Arc<AtomicBool>,
}

pub(super) struct CancellableCleanupProvider {
    pub(super) reached: Arc<Barrier>,
    pub(super) cleaned: Arc<AtomicUsize>,
    pub(super) block: AtomicBool,
}

impl Dns01Provider for CancellableCleanupProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn create_txt_record(
        &self,
        _challenge: &Dns01Challenge,
        _credentials: &Dns01Credentials,
        _operation: &Dns01Operation,
    ) -> Result<Dns01Record, Dns01ProviderError> {
        Err(Dns01ProviderError::ProviderFailed)
    }

    fn wait_for_propagation(
        &self,
        _challenge: &Dns01Challenge,
        _record: &Dns01Record,
        _credentials: &Dns01Credentials,
        _operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        Err(Dns01ProviderError::ProviderFailed)
    }

    fn cleanup_txt_record(
        &self,
        _record: &Dns01Record,
        _credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        if self.block.swap(false, Ordering::AcqRel) {
            self.reached.wait();
            let _ = operation
                .cancellation()
                .wait_timeout(Duration::from_secs(1));
        }
        operation.check()?;
        self.cleaned.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

impl Dns01Provider for FakeDnsProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn create_txt_record(
        &self,
        challenge: &Dns01Challenge,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<Dns01Record, Dns01ProviderError> {
        operation.check()?;
        assert_eq!(credentials.as_bytes(), b"dns-secret");
        self.created
            .lock()
            .expect("created records")
            .push(challenge.record_name().into());
        Dns01Record::new(
            self.name(),
            challenge.challenge_url(),
            challenge.record_name(),
            challenge.record_value().as_bytes().to_vec(),
            "fake-record-1",
        )
    }

    fn wait_for_propagation(
        &self,
        challenge: &Dns01Challenge,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        operation.check()?;
        assert_eq!(credentials.as_bytes(), b"dns-secret");
        assert!(record.matches(challenge, self.name()));
        self.propagated
            .lock()
            .expect("propagated records")
            .push(record.provider_record_id().into());
        Ok(())
    }

    fn cleanup_txt_record(
        &self,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError> {
        operation.check()?;
        assert_eq!(credentials.as_bytes(), b"dns-secret");
        if self.cleanup_fail.load(Ordering::Acquire) {
            return Err(Dns01ProviderError::CleanupFailed);
        }
        self.cleaned
            .lock()
            .expect("cleaned records")
            .push(record.provider_record_id().into());
        Ok(())
    }
}

pub(super) fn persisted_jobs(state: &StateStore) -> Vec<JobState> {
    let mut jobs = fs::read_dir(state.root().join("jobs"))
        .expect("job directory")
        .map(|entry| {
            let path = entry.expect("job entry").path();
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .expect("job ID");
            state.read_job(id).expect("persisted job")
        })
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| left.id().cmp(right.id()));
    jobs
}

pub(super) fn assert_administrative_action_job_lifecycle(
    reconciler: &AcmeManagedReconciler,
    state: &StateStore,
) {
    let revoke_job_id = reconciler
        .begin_action_job("revoke", "action:revoke")
        .expect("begin revoke action");
    assert_eq!(
        state
            .read_job(&revoke_job_id)
            .expect("running revoke")
            .status(),
        &JobStatus::Running
    );
    reconciler
        .finish_action_job(&revoke_job_id, &Ok(()), AcmeManagedOutcome::Revoked)
        .expect("finish revoke action");
    assert_eq!(
        state.read_job(&revoke_job_id).expect("revoke job").status(),
        &JobStatus::Succeeded
    );
    reconciler.set_job_id(None);

    let rollover_job_id = reconciler
        .begin_action_job("account_rollover", "action:rollover")
        .expect("begin rollover action");
    reconciler
        .finish_action_job(
            &rollover_job_id,
            &Err(AcmeManagedError::AccountNotConfigured),
            AcmeManagedOutcome::AccountKeyRolledOver,
        )
        .expect("finish failed rollover action");
    assert_eq!(
        state
            .read_job(&rollover_job_id)
            .expect("rollover job")
            .status(),
        &JobStatus::Failed
    );
    reconciler.set_job_id(None);

    let (outcome, job_id) = reconciler
        .delete_state_with_correlation("action:test")
        .expect("delete action");
    assert_eq!(outcome, AcmeManagedOutcome::Deleted);
    let job = state.read_job(&job_id).expect("delete job");
    assert_eq!(job.operation(), "delete");
    assert_eq!(job.correlation_id(), Some("action:test"));
    assert_eq!(job.status(), &JobStatus::Succeeded);
    assert_eq!(job.attempt(), 1);
}

pub(super) fn directory_response() -> HttpResponse {
    HttpResponse::new(
            200,
            "https://acme.test/directory",
            br#"{"newNonce":"https://acme.test/acme/new-nonce","newAccount":"https://acme.test/acme/new-account","newOrder":"https://acme.test/acme/new-order","meta":{"termsOfService":"https://acme.test/terms"}}"#.to_vec(),
        )
}

pub(super) fn nonce_response(nonce: &str) -> HttpResponse {
    HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
        .with_header("replay-nonce", nonce)
}

pub(super) fn account_response() -> HttpResponse {
    HttpResponse::new(
        201,
        "https://acme.test/acme/new-account",
        br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#
            .to_vec(),
    )
    .with_header("location", "https://acme.test/acme/acct/1")
    .with_header("replay-nonce", "nonce-account-response")
}

pub(super) fn problem_response(problem_type: &str) -> HttpResponse {
    HttpResponse::new(
        400,
        "https://acme.test/acme/new-order",
        format!(r#"{{"type":"urn:ietf:params:acme:error:{problem_type}"}}"#).into_bytes(),
    )
    .with_header("replay-nonce", "nonce-order-problem")
}

pub(super) fn order_response(status: &str) -> HttpResponse {
    order_response_with_identifier("proxy.example.test", status)
}

pub(super) fn order_response_with_identifier(identifier: &str, status: &str) -> HttpResponse {
    let body = format!(
        "{{\"status\":\"{status}\",\"identifiers\":[{{\"type\":\"dns\",\"value\":\"{identifier}\"}}],\"authorizations\":[\"https://acme.test/acme/authz/1\"],\"finalize\":\"https://acme.test/acme/order/1/finalize\"{}}}",
        if status == "valid" {
            ",\"certificate\":\"https://acme.test/acme/certificate/1\""
        } else {
            ""
        }
    );
    HttpResponse::new(
        if status == "pending" { 201 } else { 200 },
        "https://acme.test/acme/order/1",
        body.into_bytes(),
    )
    .with_header("replay-nonce", "nonce-order-response")
}

pub(super) fn authorization_response(status: &str) -> HttpResponse {
    let challenges = if status == "valid" {
        "[]".to_owned()
    } else {
        r#"[{"type":"http-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]"#
            .into()
    };
    let body = format!(
        "{{\"status\":\"{status}\",\"identifier\":{{\"type\":\"dns\",\"value\":\"proxy.example.test\"}},\"challenges\":{challenges}}}"
    );
    HttpResponse::new(200, "https://acme.test/acme/authz/1", body.into_bytes())
        .with_header("replay-nonce", "nonce-authz-response")
}

pub(super) fn tls_alpn_authorization_response(status: &str) -> HttpResponse {
    let challenges = if status == "valid" {
        "[]".to_owned()
    } else {
        r#"[{"type":"tls-alpn-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]"#
            .into()
    };
    let body = format!(
        "{{\"status\":\"{status}\",\"identifier\":{{\"type\":\"dns\",\"value\":\"proxy.example.test\"}},\"challenges\":{challenges}}}"
    );
    HttpResponse::new(200, "https://acme.test/acme/authz/1", body.into_bytes())
        .with_header("replay-nonce", "nonce-authz-response")
}

pub(super) fn dns_authorization_response(status: &str) -> HttpResponse {
    let challenges = if status == "valid" {
        "[]".to_owned()
    } else {
        r#"[{"type":"dns-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]"#.into()
    };
    let body = format!(
        "{{\"status\":\"{status}\",\"identifier\":{{\"type\":\"dns\",\"value\":\"*.example.test\"}},\"challenges\":{challenges}}}"
    );
    HttpResponse::new(200, "https://acme.test/acme/authz/1", body.into_bytes())
        .with_header("replay-nonce", "nonce-authz-response")
}

pub(super) fn challenge_response() -> HttpResponse {
    HttpResponse::new(
        200,
        "https://acme.test/acme/challenge/1",
        br#"{"status":"processing"}"#.to_vec(),
    )
    .with_header("replay-nonce", "nonce-challenge-response")
}

pub(super) fn test_ca() -> Result<TestCa, openssl::error::ErrorStack> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    let key = PKey::from_ec_key(EcKey::generate(&group)?)?;
    let mut subject = X509NameBuilder::new()?;
    subject.append_entry_by_text("commonName", "Fake Pebble Root")?;
    let subject = subject.build();
    let mut serial = BigNum::new()?;
    serial.rand(64, MsbOption::ONE, false)?;
    let serial = serial.to_asn1_integer()?;
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&subject)?;
    builder.set_issuer_name(&subject)?;
    builder.set_pubkey(&key)?;
    builder.set_not_before(Asn1Time::days_from_now(0)?.as_ref())?;
    builder.set_not_after(Asn1Time::days_from_now(30)?.as_ref())?;
    builder.append_extension(BasicConstraints::new().critical().ca().build()?)?;
    builder.append_extension(
        KeyUsage::new()
            .critical()
            .key_cert_sign()
            .crl_sign()
            .build()?,
    )?;
    let context = builder.x509v3_context(None, None);
    builder.append_extension(SubjectKeyIdentifier::new().build(&context)?)?;
    builder.sign(&key, MessageDigest::sha256())?;
    Ok(TestCa {
        certificate: builder.build(),
        key,
    })
}

fn issue_certificate(
    csr: &X509Req,
    ca: &TestCa,
    certificate_names: &[String],
) -> Result<Vec<u8>, openssl::error::ErrorStack> {
    let public_key = csr.public_key()?;
    let mut serial = BigNum::new()?;
    serial.rand(64, MsbOption::ONE, false)?;
    let serial = serial.to_asn1_integer()?;
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(csr.subject_name())?;
    builder.set_issuer_name(ca.certificate.subject_name())?;
    builder.set_pubkey(&public_key)?;
    builder.set_not_before(Asn1Time::days_from_now(0)?.as_ref())?;
    builder.set_not_after(Asn1Time::days_from_now(30)?.as_ref())?;
    let context = builder.x509v3_context(Some(&ca.certificate), None);
    let mut subject_alt_names = SubjectAlternativeName::new();
    for name in certificate_names {
        subject_alt_names.dns(name);
    }
    builder.append_extension(subject_alt_names.build(&context)?)?;
    let context = builder.x509v3_context(Some(&ca.certificate), None);
    builder.append_extension(
        AuthorityKeyIdentifier::new()
            .keyid(true)
            .issuer(true)
            .build(&context)?,
    )?;
    builder.append_extension(KeyUsage::new().critical().digital_signature().build()?)?;
    builder.append_extension(ExtendedKeyUsage::new().server_auth().build()?)?;
    builder.sign(&ca.key, MessageDigest::sha256())?;
    let leaf = builder.build();
    let mut chain = leaf.to_pem()?;
    chain.extend_from_slice(&ca.certificate.to_pem()?);
    Ok(chain)
}
