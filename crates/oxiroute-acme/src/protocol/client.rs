#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug)]
pub struct PollPolicy {
    pub max_attempts: usize,
    pub deadline_unix_seconds: u64,
    pub initial_delay_seconds: u64,
    pub max_delay_seconds: u64,
    pub cancellation: Option<Dns01Cancellation>,
}

impl Default for PollPolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_POLL_ATTEMPTS,
            deadline_unix_seconds: u64::MAX,
            initial_delay_seconds: 1,
            max_delay_seconds: MAX_POLL_DELAY_SECONDS,
            cancellation: None,
        }
    }
}

#[derive(Clone)]
pub struct AcmeClient<T> {
    transport: T,
    directory: Directory,
    pub(super) key: AccountKey,
    policy: OriginPolicy,
    clock: Arc<dyn Clock>,
    pub(super) nonces: VecDeque<String>,
    account: Option<Account>,
    operation: AcmeOperation,
}

impl<T> fmt::Debug for AcmeClient<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcmeClient")
            .field("directory", &self.directory.url)
            .field("account", &self.account)
            .field("nonce_count", &self.nonces.len())
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<T: AcmeTransport> AcmeClient<T> {
    /// Fetches a directory and constructs a client with an injected clock and transport.
    ///
    /// # Errors
    ///
    /// Returns an error when directory validation or key setup fails.
    pub fn new(
        transport: T,
        directory_url: &str,
        policy: OriginPolicy,
        key: AccountKey,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AcmeError> {
        Self::new_with_operation(
            transport,
            directory_url,
            policy,
            key,
            clock,
            AcmeOperation::default(),
        )
    }

    /// Constructs a client while applying cancellation to the initial directory request.
    ///
    /// # Errors
    ///
    /// Returns the same setup errors as [`Self::new`], plus [`AcmeError::Cancelled`] when
    /// cancellation or the operation deadline wins.
    pub fn new_with_operation(
        transport: T,
        directory_url: &str,
        policy: OriginPolicy,
        key: AccountKey,
        clock: Arc<dyn Clock>,
        operation: AcmeOperation,
    ) -> Result<Self, AcmeError> {
        let directory =
            Directory::fetch_with_operation(&transport, directory_url, &policy, &operation)?;
        Ok(Self {
            transport,
            directory,
            key,
            policy,
            clock,
            nonces: VecDeque::new(),
            account: None,
            operation,
        })
    }

    #[must_use]
    pub const fn directory(&self) -> &Directory {
        &self.directory
    }

    #[must_use]
    pub const fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    /// Associates an already registered account with this client after validating its endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the persisted account URL is outside the directory origin policy.
    pub fn set_account(&mut self, account: Account) -> Result<(), AcmeError> {
        self.policy.permits(&account.url)?;
        validate_account_status(&account.status)?;
        validate_account_contacts(&account.contacts)?;
        if !account.terms_agreed {
            return Err(AcmeError::TermsNotAgreed);
        }
        self.account = Some(account);
        Ok(())
    }

    /// Registers an account once, refusing ambiguous replay after a transport failure.
    ///
    /// # Errors
    ///
    /// Returns an error when terms, contacts, external account binding, or the ACME response is
    /// invalid. A transport error is returned as an ambiguous result and is not retried.
    pub fn register_account(&mut self, request: &AccountRequest) -> Result<Account, AcmeError> {
        validate_account_request(request)?;
        if self.directory.document.external_account_required {
            return Err(AcmeError::ExternalAccountBindingRequired);
        }
        if self.directory.document.terms_of_service.is_some() && !request.terms_agreed {
            return Err(AcmeError::TermsNotAgreed);
        }
        let payload = json!({
            "termsOfServiceAgreed": request.terms_agreed,
            "contact": request.contacts,
        });
        let account_url = self.directory.document.new_account.clone();
        let response = self
            .signed_request(&account_url, &payload, ProtectedKey::Jwk)
            .map_err(|error| match error {
                AcmeError::Transport(_) => AcmeError::AccountRegistrationAmbiguous,
                other => other,
            })?;
        if response.status != 201 {
            return Err(problem_or_status(&response));
        }
        let url = response
            .header("location")
            .ok_or(AcmeError::MissingField)?
            .to_owned();
        let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
        let account = parse_account(&value, url, &self.policy, request.terms_agreed)?;
        self.account = Some(account.clone());
        Ok(account)
    }

    /// Revokes one leaf certificate using the configured account.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory does not advertise revocation, the PEM is malformed,
    /// the reason is unsupported, or the ACME response is unsuccessful.
    pub fn revoke_certificate(
        &mut self,
        certificate_pem: &[u8],
        reason: Option<u8>,
    ) -> Result<(), AcmeError> {
        if reason.is_some_and(|reason| reason > 8) {
            return Err(AcmeError::InvalidRevocationReason);
        }
        let endpoint = self
            .directory
            .document
            .revoke_certificate
            .as_deref()
            .ok_or(AcmeError::RevocationUnsupported)?
            .to_owned();
        let certificates =
            X509::stack_from_pem(certificate_pem).map_err(|_| AcmeError::InvalidCertificate)?;
        let [certificate] = certificates.as_slice() else {
            return Err(AcmeError::InvalidCertificate);
        };
        let certificate = certificate
            .to_der()
            .map_err(|_| AcmeError::InvalidCertificate)?;
        let mut payload = serde_json::Map::new();
        payload.insert(
            "certificate".into(),
            Value::String(URL_SAFE_NO_PAD.encode(certificate)),
        );
        if let Some(reason) = reason {
            payload.insert("reason".into(), Value::from(reason));
        }
        let response = self.signed_account_request(&endpoint, &Value::Object(payload))?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        Ok(())
    }

    /// Performs RFC 8555 account key rollover and installs the new key only after the CA accepts it.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory does not advertise key rollover, the account response is
    /// malformed, or the nested JWS request is rejected.
    pub fn rollover_account_key(&mut self, new_key: AccountKey) -> Result<Account, AcmeError> {
        let endpoint = self
            .directory
            .document
            .key_change
            .as_deref()
            .ok_or(AcmeError::KeyChangeUnsupported)?
            .to_owned();
        let account = self.account.clone().ok_or(AcmeError::MissingField)?;
        let inner_payload = json!({
            "account": account.url,
            "oldKey": self.key.jwk(),
        });
        let response =
            self.key_change_request(&endpoint, &inner_payload, &new_key, &account.url)?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
        let replacement = parse_account(&value, account.url, &self.policy, account.terms_agreed)?;
        self.key = new_key;
        self.account = Some(replacement.clone());
        Ok(replacement)
    }

    /// Creates an order for exactly the normalized configured DNS identifier set.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported identifiers, endpoint policy violations, or malformed ACME
    /// order responses.
    pub fn create_order(&mut self, request: &CertificateRequest) -> Result<Order, AcmeError> {
        let identifiers = normalize_identifiers(&request.identifiers)?;
        let payload = json!({
            "identifiers": identifiers.iter().map(|value| json!({ "type": "dns", "value": value })).collect::<Vec<_>>(),
        });
        let order_url = self.directory.document.new_order.clone();
        let response = self.signed_account_request(&order_url, &payload)?;
        if response.status != 201 {
            return Err(problem_or_status(&response));
        }
        let order = parse_order(&response, &self.policy)?;
        if order.identifiers != identifiers {
            return Err(AcmeError::OrderIdentifiersMismatch);
        }
        if !matches!(order.status.as_str(), "pending" | "ready") {
            return Err(AcmeError::InvalidOrderState {
                status: order.status,
            });
        }
        Ok(order)
    }

    /// Loads an authorization and selects its exact HTTP-01 challenge.
    ///
    /// # Errors
    ///
    /// Returns an error when the authorization is malformed, outside policy, or lacks HTTP-01.
    pub fn authorization(&mut self, url: &str) -> Result<Authorization, AcmeError> {
        self.authorization_response(url, ChallengeType::Http01)
            .map(|(authorization, _)| authorization)
    }

    /// Loads an authorization and selects its exact challenge type.
    ///
    /// # Errors
    ///
    /// Returns an error when the authorization is malformed, outside policy, or lacks the selected
    /// challenge type while it is still pending.
    pub fn authorization_for(
        &mut self,
        url: &str,
        challenge_type: ChallengeType,
    ) -> Result<Authorization, AcmeError> {
        self.authorization_response(url, challenge_type)
            .map(|(authorization, _)| authorization)
    }

    fn authorization_response(
        &mut self,
        url: &str,
        challenge_type: ChallengeType,
    ) -> Result<(Authorization, HttpResponse), AcmeError> {
        let response = self.post_as_get(url)?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        let authorization =
            parse_authorization(&response, url, &self.key, &self.policy, challenge_type)?;
        Ok((authorization, response))
    }

    /// Notifies the CA that an HTTP-01 challenge is provisioned.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed challenge URL or ACME failure.
    pub fn respond_to_challenge(&mut self, challenge: &Http01Challenge) -> Result<(), AcmeError> {
        self.respond_to_challenge_url(&challenge.challenge_url)
    }

    /// Notifies the CA that a TLS-ALPN-01 challenge is provisioned.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed challenge URL or ACME failure.
    pub fn respond_to_tls_alpn01_challenge(
        &mut self,
        challenge: &TlsAlpn01Challenge,
    ) -> Result<(), AcmeError> {
        self.respond_to_challenge_url(&challenge.challenge_url)
    }

    /// Notifies the CA that a DNS-01 TXT record is provisioned.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed challenge URL or ACME failure.
    pub fn respond_to_dns01_challenge(
        &mut self,
        challenge: &Dns01Challenge,
    ) -> Result<(), AcmeError> {
        self.respond_to_challenge_url(challenge.challenge_url())
    }

    fn respond_to_challenge_url(&mut self, challenge_url: &str) -> Result<(), AcmeError> {
        let response = self.signed_account_request(challenge_url, &json!({}))?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        Ok(())
    }

    /// Polls an authorization with bounded attempts, deadline, backoff, and Retry-After support.
    ///
    /// # Errors
    ///
    /// Returns an error when a terminal invalid state, malformed response, invalid retry guidance,
    /// transport failure, or bounded timeout occurs.
    pub fn poll_authorization(
        &mut self,
        url: &str,
        poll: &PollPolicy,
    ) -> Result<Authorization, AcmeError> {
        self.poll_authorization_for(url, poll, ChallengeType::Http01)
    }

    /// Polls an authorization for a selected challenge type with bounded attempts, deadline,
    /// backoff, and Retry-After support.
    ///
    /// # Errors
    ///
    /// Returns an error when a terminal invalid state, malformed response, invalid retry guidance,
    /// transport failure, or bounded timeout occurs.
    pub fn poll_authorization_for(
        &mut self,
        url: &str,
        poll: &PollPolicy,
        challenge_type: ChallengeType,
    ) -> Result<Authorization, AcmeError> {
        let clock = Arc::clone(&self.clock);
        poll_acme(clock.as_ref(), url, poll, || {
            let (authorization, response) = self.authorization_response(url, challenge_type)?;
            match authorization.status {
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Deactivated
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Valid => Ok(PollAttempt::Complete(authorization)),
                AuthorizationStatus::Pending | AuthorizationStatus::Processing => {
                    Ok(PollAttempt::Pending(response))
                }
            }
        })
    }

    /// Finalizes an order with a DER CSR and returns its next server state.
    ///
    /// # Errors
    ///
    /// Returns an error for an absent finalize URL, policy violation, or malformed response.
    pub fn finalize_order(&mut self, order: &Order, csr_der: &[u8]) -> Result<Order, AcmeError> {
        let finalize = order.finalize.as_deref().ok_or(AcmeError::MissingField)?;
        let payload = json!({ "csr": URL_SAFE_NO_PAD.encode(csr_der) });
        let response = self.signed_account_request(finalize, &payload)?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        parse_order(&response, &self.policy)
    }

    /// Polls an order until it is valid or reaches a terminal invalid state.
    ///
    /// # Errors
    ///
    /// Returns an error when polling exceeds its bounds or the order response is invalid.
    pub fn poll_order(&mut self, url: &str, poll: &PollPolicy) -> Result<Order, AcmeError> {
        let clock = Arc::clone(&self.clock);
        poll_acme(clock.as_ref(), url, poll, || {
            let response = self.post_as_get(url)?;
            if response.status != 200 {
                return Err(problem_or_status(&response));
            }
            let order = parse_order(&response, &self.policy)?;
            match order.status.as_str() {
                "valid" | "invalid" => Ok(PollAttempt::Complete(order)),
                "pending" | "ready" | "processing" => Ok(PollAttempt::Pending(response)),
                status => Err(AcmeError::InvalidOrderState {
                    status: status.into(),
                }),
            }
        })
    }

    /// Downloads the final certificate chain as bounded opaque PEM bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for endpoint policy violations, ACME failures, redirects, or oversized
    /// certificate material.
    pub fn download_certificate(&mut self, url: &str) -> Result<Vec<u8>, AcmeError> {
        let response = self.post_as_get(url)?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        if response.body.len() > MAX_ACME_BODY_BYTES {
            return Err(AcmeError::ResponseTooLarge {
                limit: MAX_ACME_BODY_BYTES,
            });
        }
        Ok(response.body)
    }

    /// Fetches RFC 9773 Renewal Information for exactly one leaf certificate.
    ///
    /// # Errors
    ///
    /// Returns an error when the certificate, endpoint, response, or suggested window is invalid.
    pub fn renewal_information(
        &mut self,
        certificate_pem: &[u8],
    ) -> Result<Option<RenewalInformation>, AcmeError> {
        let Some(endpoint) = self.directory.document.renewal_info.clone() else {
            return Ok(None);
        };
        let certificates =
            X509::stack_from_pem(certificate_pem).map_err(|_| AcmeError::InvalidCertificate)?;
        let [certificate] = certificates.as_slice() else {
            return Err(AcmeError::InvalidCertificate);
        };
        let certificate = certificate
            .to_der()
            .map_err(|_| AcmeError::InvalidCertificate)?;
        let certificate_id = URL_SAFE_NO_PAD.encode(Sha256::digest(&certificate));
        let endpoint = renewal_information_endpoint(&endpoint, &certificate_id)?;
        let response = self.post_as_get(&endpoint)?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
        let window = value
            .get("suggestedWindow")
            .and_then(Value::as_object)
            .ok_or(AcmeError::InvalidRenewalInformation)?;
        let start = parse_renewal_timestamp(
            window
                .get("start")
                .and_then(Value::as_str)
                .ok_or(AcmeError::InvalidRenewalInformation)?,
        )?;
        let end = parse_renewal_timestamp(
            window
                .get("end")
                .and_then(Value::as_str)
                .ok_or(AcmeError::InvalidRenewalInformation)?,
        )?;
        if end <= start || end.saturating_sub(start) > MAX_RENEWAL_INFORMATION_WINDOW_SECONDS {
            return Err(AcmeError::InvalidRenewalInformation);
        }
        Ok(Some(RenewalInformation {
            suggested_window_start_unix_seconds: start,
            suggested_window_end_unix_seconds: end,
        }))
    }

    fn signed_account_request(
        &mut self,
        url: &str,
        payload: &Value,
    ) -> Result<HttpResponse, AcmeError> {
        let account_url = self
            .account
            .as_ref()
            .ok_or(AcmeError::MissingField)?
            .url
            .clone();
        self.signed_request(url, payload, ProtectedKey::Kid(&account_url))
    }

    fn post_as_get(&mut self, url: &str) -> Result<HttpResponse, AcmeError> {
        self.signed_account_request(url, &Value::Null)
    }

    fn signed_request(
        &mut self,
        url: &str,
        payload: &Value,
        protected_key: ProtectedKey<'_>,
    ) -> Result<HttpResponse, AcmeError> {
        self.policy.permits(url)?;
        self.request_with_bad_nonce_retry(url, |client, nonce| {
            client.jws(url, nonce, payload, protected_key)
        })
    }

    fn jws(
        &self,
        url: &str,
        nonce: String,
        payload: &Value,
        protected_key: ProtectedKey<'_>,
    ) -> Result<Vec<u8>, AcmeError> {
        Self::jws_with_key(&self.key, url, nonce, payload, protected_key)
    }

    fn jws_with_key(
        key: &AccountKey,
        url: &str,
        nonce: String,
        payload: &Value,
        protected_key: ProtectedKey<'_>,
    ) -> Result<Vec<u8>, AcmeError> {
        let mut protected = BTreeMap::new();
        protected.insert("alg", Value::String(key.protected_algorithm().into()));
        protected.insert("nonce", Value::String(nonce));
        protected.insert("url", Value::String(url.into()));
        match protected_key {
            ProtectedKey::Jwk => {
                protected.insert(
                    "jwk",
                    serde_json::to_value(key.jwk()).map_err(|_| AcmeError::MalformedResponse)?,
                );
            }
            ProtectedKey::Kid(kid) => {
                protected.insert("kid", Value::String(kid.into()));
            }
        }
        let protected = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&protected).map_err(|_| AcmeError::MalformedResponse)?);
        let payload = if payload.is_null() {
            String::new()
        } else {
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(payload).map_err(|_| AcmeError::MalformedResponse)?)
        };
        let signing_input = format!("{protected}.{payload}");
        let signature = URL_SAFE_NO_PAD.encode(key.sign(signing_input.as_bytes())?);
        serde_json::to_vec(&json!({
            "protected": protected,
            "payload": payload,
            "signature": signature,
        }))
        .map_err(|_| AcmeError::MalformedResponse)
    }

    fn key_change_request(
        &mut self,
        url: &str,
        inner_payload: &Value,
        new_key: &AccountKey,
        account_url: &str,
    ) -> Result<HttpResponse, AcmeError> {
        self.request_with_bad_nonce_retry(url, |client, nonce| {
            let inner = Self::jws_with_key(
                new_key,
                url,
                nonce.clone(),
                inner_payload,
                ProtectedKey::Jwk,
            )?;
            let inner = serde_json::from_slice::<Value>(&inner)
                .map_err(|_| AcmeError::MalformedResponse)?;
            client.jws(url, nonce, &inner, ProtectedKey::Kid(account_url))
        })
    }

    fn request_with_bad_nonce_retry(
        &mut self,
        url: &str,
        mut build_body: impl FnMut(&Self, String) -> Result<Vec<u8>, AcmeError>,
    ) -> Result<HttpResponse, AcmeError> {
        let mut retried_bad_nonce = false;
        loop {
            let nonce = self.take_nonce()?;
            let body = build_body(self, nonce)?;
            if body.len() > MAX_ACME_BODY_BYTES {
                return Err(AcmeError::RequestTooLarge {
                    limit: MAX_ACME_BODY_BYTES,
                });
            }
            let mut request = HttpRequest::new("POST", url, body);
            request
                .headers
                .insert("content-type".into(), "application/jose+json".into());
            let response = self
                .transport
                .request_with_operation(request, &self.operation)?;
            validate_response_url(url, &response, &self.policy)?;
            self.record_nonce(&response)?;
            if is_bad_nonce(&response) {
                if retried_bad_nonce {
                    return Err(AcmeError::BadNonceRetryExhausted);
                }
                retried_bad_nonce = true;
                continue;
            }
            return Ok(response);
        }
    }

    fn take_nonce(&mut self) -> Result<String, AcmeError> {
        if let Some(nonce) = self.nonces.pop_front() {
            return Ok(nonce);
        }
        let response = self
            .transport
            .request(HttpRequest::new(
                "HEAD",
                &self.directory.document.new_nonce,
                Vec::new(),
            ))
            .map_err(AcmeError::Transport)?;
        validate_response_url(&self.directory.document.new_nonce, &response, &self.policy)?;
        if response.status != 200 && response.status != 204 {
            return Err(problem_or_status(&response));
        }
        let nonce = response
            .header("replay-nonce")
            .ok_or(AcmeError::MissingNonce)?
            .to_owned();
        validate_nonce(&nonce)?;
        Ok(nonce)
    }

    fn record_nonce(&mut self, response: &HttpResponse) -> Result<(), AcmeError> {
        let Some(nonce) = response.header("replay-nonce") else {
            return Ok(());
        };
        validate_nonce(nonce)?;
        if self.nonces.len() == MAX_NONCES {
            self.nonces.pop_front();
        }
        self.nonces.push_back(nonce.into());
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ProtectedKey<'a> {
    Jwk,
    Kid(&'a str),
}
