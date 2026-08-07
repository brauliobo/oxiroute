use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    net::IpAddr,
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use openssl::{
    bn::BigNumContext,
    ec::{EcGroup, EcKey, PointConversionForm},
    ecdsa::EcdsaSig,
    error::ErrorStack,
    hash::MessageDigest,
    nid::Nid,
    pkey::{Id, PKey, Private},
    rsa::Rsa,
    sign::Signer,
    stack::Stack,
    x509::{X509, X509NameBuilder, X509Req, X509ReqBuilder, extension::SubjectAlternativeName},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{Clock, Dns01Cancellation, Dns01Challenge, SecretBytes};

pub const MAX_ACME_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ACME_URL_BYTES: usize = 2_048;
pub const MAX_NONCES: usize = 32;
pub const MAX_IDENTIFIERS: usize = 100;
pub const MAX_POLL_ATTEMPTS: usize = 64;
pub const MAX_POLL_DELAY_SECONDS: u64 = 300;
pub const MAX_RENEWAL_INFORMATION_WINDOW_SECONDS: u64 =
    crate::clock::MAX_RENEWAL_INFORMATION_WINDOW_SECONDS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    fn new(method: &str, url: &str, body: Vec<u8>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    #[must_use]
    pub fn new(status: u16, url: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            status,
            url: url.into(),
            headers: BTreeMap::new(),
            body,
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub trait AcmeTransport: Send + Sync {
    /// Sends one bounded ACME request without following redirects.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the injected transport cannot produce a response.
    fn request(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("ACME transport request failed")]
pub struct TransportError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcmeFailureClass {
    Permanent,
    Retryable,
}

#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    #[error("ACME directory URL is invalid or not HTTPS")]
    InvalidDirectoryUrl,
    #[error("ACME endpoint is outside the configured outbound origin policy")]
    EndpointOutsidePolicy,
    #[error("ACME endpoint uses an untrusted redirect")]
    UntrustedRedirect,
    #[error("ACME private or local origin requires an explicit development allowlist")]
    PrivateOriginRequiresAllowlist,
    #[error("ACME response exceeds the {limit}-byte bound")]
    ResponseTooLarge { limit: usize },
    #[error("ACME transport failed")]
    Transport(#[source] TransportError),
    #[error("ACME response returned unexpected HTTP status {status}")]
    UnexpectedStatus { status: u16 },
    #[error("ACME response is malformed")]
    MalformedResponse,
    #[error("ACME response is missing a required field")]
    MissingField,
    #[error("ACME problem type `{problem_type}` was returned")]
    Problem { problem_type: String },
    #[error("ACME replay nonce is missing")]
    MissingNonce,
    #[error("ACME replay nonce pool is exhausted")]
    NoncePoolExhausted,
    #[error("ACME returned badNonce after its one permitted retry")]
    BadNonceRetryExhausted,
    #[error("ACME account registration is ambiguous and was not replayed")]
    AccountRegistrationAmbiguous,
    #[error("ACME account contact policy is invalid")]
    InvalidAccountRequest,
    #[error("ACME account is not usable: {status}")]
    AccountNotUsable { status: String },
    #[error("ACME terms of service must be explicitly agreed")]
    TermsNotAgreed,
    #[error("ACME directory requires external account binding")]
    ExternalAccountBindingRequired,
    #[error("ACME identifier set is invalid or unsupported")]
    UnsupportedIdentifier,
    #[error("ACME IP identifiers are unsupported by the managed issuance path")]
    IpIdentifierUnsupported,
    #[error("ACME order identifiers do not match the requested set")]
    OrderIdentifiersMismatch,
    #[error("ACME order returned an unsupported state: {status}")]
    InvalidOrderState { status: String },
    #[error("ACME authorization has no supported challenge")]
    UnsupportedChallenge,
    #[error("ACME polling exceeded its bounded deadline")]
    PollTimeout,
    #[error("ACME operation was cancelled")]
    Cancelled,
    #[error("ACME retry guidance is invalid")]
    InvalidRetryAfter,
    #[error("ACME request exceeds the {limit}-byte bound")]
    RequestTooLarge { limit: usize },
    #[error("ACME account key could not be generated or used")]
    Key(#[source] ErrorStack),
    #[error("ACME directory does not advertise certificate revocation")]
    RevocationUnsupported,
    #[error("ACME directory does not advertise account key rollover")]
    KeyChangeUnsupported,
    #[error("ACME certificate PEM is invalid")]
    InvalidCertificate,
    #[error("ACME revocation reason is invalid")]
    InvalidRevocationReason,
    #[error("ACME Renewal Information response is invalid")]
    InvalidRenewalInformation,
    #[error("ACME JWS signature could not be created")]
    Signature(#[source] ErrorStack),
    #[error("ACME CSR could not be created")]
    Csr(#[source] ErrorStack),
}

impl AcmeError {
    #[must_use]
    fn failure_class(&self) -> AcmeFailureClass {
        match self {
            Self::Transport(_)
            | Self::MissingNonce
            | Self::NoncePoolExhausted
            | Self::BadNonceRetryExhausted
            | Self::PollTimeout
            | Self::InvalidRetryAfter => AcmeFailureClass::Retryable,
            Self::UnexpectedStatus { status }
                if matches!(*status, 408 | 425 | 429 | 500 | 502 | 503 | 504) =>
            {
                AcmeFailureClass::Retryable
            }
            Self::Problem { problem_type } if retryable_problem_type(problem_type) => {
                AcmeFailureClass::Retryable
            }
            _ => AcmeFailureClass::Permanent,
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.failure_class() == AcmeFailureClass::Retryable
    }
}

fn retryable_problem_type(problem_type: &str) -> bool {
    matches!(
        problem_type.rsplit(':').next(),
        Some("badNonce" | "connection" | "dns" | "rateLimited" | "serverInternal" | "tls")
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginPolicy {
    allowed_origins: BTreeSet<String>,
}

impl OriginPolicy {
    /// Creates a policy for a public HTTPS directory origin.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, non-HTTPS, or local origins.
    pub fn strict(directory_url: &str) -> Result<Self, AcmeError> {
        let origin = origin_of(directory_url)?;
        if is_private_origin(&origin) {
            return Err(AcmeError::PrivateOriginRequiresAllowlist);
        }
        Ok(Self {
            allowed_origins: [origin].into_iter().collect(),
        })
    }

    /// Creates an explicitly allowlisted policy for deterministic local or private tests.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or non-HTTPS URLs.
    pub fn development_allowlist(directory_url: &str) -> Result<Self, AcmeError> {
        Ok(Self {
            allowed_origins: [origin_of(directory_url)?].into_iter().collect(),
        })
    }

    /// Adds one advertised HTTPS origin to the explicit outbound allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or non-HTTPS URLs.
    pub fn allow_origin(mut self, url: &str) -> Result<Self, AcmeError> {
        self.allowed_origins.insert(origin_of(url)?);
        Ok(self)
    }

    fn permits(&self, url: &str) -> Result<(), AcmeError> {
        if url.len() > MAX_ACME_URL_BYTES || !self.allowed_origins.contains(&origin_of(url)?) {
            return Err(AcmeError::EndpointOutsidePolicy);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryDocument {
    pub new_nonce: String,
    pub new_account: String,
    pub new_order: String,
    pub revoke_certificate: Option<String>,
    pub key_change: Option<String>,
    pub renewal_info: Option<String>,
    pub terms_of_service: Option<String>,
    pub external_account_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directory {
    pub url: String,
    pub document: DirectoryDocument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenewalInformation {
    pub suggested_window_start_unix_seconds: u64,
    pub suggested_window_end_unix_seconds: u64,
}

impl Directory {
    /// Fetches and validates an ACME v2 directory through an injected transport.
    ///
    /// # Errors
    ///
    /// Returns an error for an untrusted origin, redirect, oversized body, malformed document, or
    /// transport failure.
    pub fn fetch<T: AcmeTransport>(
        transport: &T,
        url: &str,
        policy: &OriginPolicy,
    ) -> Result<Self, AcmeError> {
        policy.permits(url)?;
        let request = HttpRequest::new("GET", url, Vec::new());
        let response = transport.request(request).map_err(AcmeError::Transport)?;
        validate_response_url(url, &response, policy)?;
        if response.status / 100 == 3 {
            return Err(AcmeError::UntrustedRedirect);
        }
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
        let document = DirectoryDocument {
            new_nonce: required_string(&value, "newNonce")?,
            new_account: required_string(&value, "newAccount")?,
            new_order: required_string(&value, "newOrder")?,
            revoke_certificate: optional_string(&value, "revokeCert")?,
            key_change: optional_string(&value, "keyChange")?,
            renewal_info: optional_string(&value, "renewalInfo")?,
            terms_of_service: value
                .get("meta")
                .and_then(|meta| meta.get("termsOfService"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            external_account_required: value
                .get("meta")
                .and_then(|meta| meta.get("externalAccountRequired"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        for endpoint in [
            Some(document.new_nonce.as_str()),
            Some(document.new_account.as_str()),
            Some(document.new_order.as_str()),
            document.revoke_certificate.as_deref(),
            document.key_change.as_deref(),
            document.renewal_info.as_deref(),
            document.terms_of_service.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            policy.permits(endpoint)?;
        }
        Ok(Self {
            url: url.into(),
            document,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountKeyAlgorithm {
    EcdsaP256,
    Rsa2048,
}

#[derive(Clone)]
pub struct AccountKey {
    private_key_pem: SecretBytes,
    algorithm: AccountKeyAlgorithm,
    jwk: BTreeMap<String, String>,
}

impl fmt::Debug for AccountKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountKey")
            .field("algorithm", &self.algorithm)
            .field("thumbprint", &self.thumbprint())
            .field("private_key", &"REDACTED")
            .finish_non_exhaustive()
    }
}

impl AccountKey {
    /// Generates an ACME account key with the selected maintained-library algorithm.
    ///
    /// # Errors
    ///
    /// Returns an error when OpenSSL cannot generate or serialize the key.
    pub fn generate(algorithm: AccountKeyAlgorithm) -> Result<Self, AcmeError> {
        let private_key = match algorithm {
            AccountKeyAlgorithm::EcdsaP256 => {
                let group =
                    EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).map_err(AcmeError::Key)?;
                PKey::from_ec_key(EcKey::generate(&group).map_err(AcmeError::Key)?)
                    .map_err(AcmeError::Key)?
            }
            AccountKeyAlgorithm::Rsa2048 => {
                PKey::from_rsa(Rsa::generate(2_048).map_err(AcmeError::Key)?)
                    .map_err(AcmeError::Key)?
            }
        };
        let private_key_pem = private_key
            .private_key_to_pem_pkcs8()
            .map_err(AcmeError::Key)?;
        let jwk = public_jwk(&private_key, algorithm)?;
        Ok(Self {
            private_key_pem: SecretBytes::new(private_key_pem),
            algorithm,
            jwk,
        })
    }

    /// Loads an unencrypted account key from owner-protected PEM bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is unsupported, weak, malformed, or cannot produce its JWK.
    pub fn from_pem(private_key_pem: SecretBytes) -> Result<Self, AcmeError> {
        let key = PKey::private_key_from_pem(private_key_pem.as_bytes()).map_err(AcmeError::Key)?;
        let algorithm = match key.id() {
            Id::EC if key.bits() >= 256 => AccountKeyAlgorithm::EcdsaP256,
            Id::RSA | Id::RSA_PSS if key.bits() >= 2_048 => AccountKeyAlgorithm::Rsa2048,
            _ => return Err(AcmeError::MalformedResponse),
        };
        Ok(Self {
            jwk: public_jwk(&key, algorithm)?,
            private_key_pem,
            algorithm,
        })
    }

    #[must_use]
    pub const fn algorithm(&self) -> AccountKeyAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn private_key_pem(&self) -> &SecretBytes {
        &self.private_key_pem
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics only if the fixed string-valued JWK representation cannot be serialized.
    pub fn thumbprint(&self) -> String {
        let bytes = serde_json::to_vec(&self.jwk).expect("string-valued JWK serializes");
        URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes))
    }

    #[must_use]
    pub fn key_authorization(&self, token: &str) -> String {
        format!("{token}.{}", self.thumbprint())
    }

    fn protected_algorithm(&self) -> &'static str {
        match self.algorithm {
            AccountKeyAlgorithm::EcdsaP256 => "ES256",
            AccountKeyAlgorithm::Rsa2048 => "RS256",
        }
    }

    fn jwk(&self) -> &BTreeMap<String, String> {
        &self.jwk
    }

    fn sign(&self, input: &[u8]) -> Result<Vec<u8>, AcmeError> {
        let key =
            PKey::private_key_from_pem(self.private_key_pem.as_bytes()).map_err(AcmeError::Key)?;
        let digest = MessageDigest::sha256();
        let mut signer = Signer::new(digest, &key).map_err(AcmeError::Signature)?;
        signer.update(input).map_err(AcmeError::Signature)?;
        let signature = signer.sign_to_vec().map_err(AcmeError::Signature)?;
        if self.algorithm != AccountKeyAlgorithm::EcdsaP256 {
            return Ok(signature);
        }
        let signature = EcdsaSig::from_der(&signature).map_err(AcmeError::Signature)?;
        let mut raw = Vec::with_capacity(64);
        raw.extend(
            signature
                .r()
                .to_vec_padded(32)
                .map_err(AcmeError::Signature)?,
        );
        raw.extend(
            signature
                .s()
                .to_vec_padded(32)
                .map_err(AcmeError::Signature)?,
        );
        Ok(raw)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountRequest {
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub url: String,
    pub status: String,
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateRequest {
    pub identifiers: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub url: String,
    pub status: String,
    pub identifiers: Vec<String>,
    pub authorizations: Vec<String>,
    pub finalize: Option<String>,
    pub certificate: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChallengeType {
    Http01,
    Dns01,
    TlsAlpn01,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationStatus {
    Pending,
    Processing,
    Valid,
    Invalid,
    Deactivated,
    Expired,
    Revoked,
}

impl AuthorizationStatus {
    fn parse(value: &str) -> Result<Self, AcmeError> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "valid" => Ok(Self::Valid),
            "invalid" => Ok(Self::Invalid),
            "deactivated" => Ok(Self::Deactivated),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(AcmeError::MalformedResponse),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Authorization {
    pub url: String,
    pub identifier: String,
    pub status: AuthorizationStatus,
    pub challenge: Option<Http01Challenge>,
    pub dns01_challenge: Option<Dns01Challenge>,
    pub tls_alpn01_challenge: Option<TlsAlpn01Challenge>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Http01Challenge {
    pub authorization_url: String,
    pub challenge_url: String,
    pub token: String,
    pub key_authorization: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsAlpn01Challenge {
    pub authorization_url: String,
    pub challenge_url: String,
    pub token: String,
    pub key_authorization: String,
}

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
    key: AccountKey,
    policy: OriginPolicy,
    clock: Arc<dyn Clock>,
    nonces: VecDeque<String>,
    account: Option<Account>,
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
        let directory = Directory::fetch(&transport, directory_url, &policy)?;
        Ok(Self {
            transport,
            directory,
            key,
            policy,
            clock,
            nonces: VecDeque::new(),
            account: None,
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
        let response = self.signed_account_request(&challenge.challenge_url, &json!({}))?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        Ok(())
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
        let response = self.signed_account_request(&challenge.challenge_url, &json!({}))?;
        if response.status != 200 {
            return Err(problem_or_status(&response));
        }
        Ok(())
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
        let response = self.signed_account_request(challenge.challenge_url(), &json!({}))?;
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
        let (max_attempts, mut delay, max_delay) = bounded_poll_policy(poll);
        for attempt in 0..max_attempts {
            if poll
                .cancellation
                .as_ref()
                .is_some_and(Dns01Cancellation::is_cancelled)
            {
                return Err(AcmeError::Cancelled);
            }
            if self.clock.now_unix_seconds() > poll.deadline_unix_seconds {
                return Err(AcmeError::PollTimeout);
            }
            let (authorization, response) = self.authorization_response(url, challenge_type)?;
            match authorization.status {
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Deactivated
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Valid => return Ok(authorization),
                AuthorizationStatus::Pending | AuthorizationStatus::Processing => {}
            }
            if poll
                .cancellation
                .as_ref()
                .is_some_and(Dns01Cancellation::is_cancelled)
            {
                return Err(AcmeError::Cancelled);
            }
            if attempt + 1 == max_attempts {
                return Err(AcmeError::PollTimeout);
            }
            let effective_delay = retry_after(&response, self.clock.now_unix_seconds())?
                .unwrap_or_else(|| jittered_delay(delay, max_delay, url, attempt));
            if self
                .clock
                .now_unix_seconds()
                .saturating_add(effective_delay)
                > poll.deadline_unix_seconds
            {
                return Err(AcmeError::PollTimeout);
            }
            if poll
                .cancellation
                .as_ref()
                .is_some_and(Dns01Cancellation::is_cancelled)
            {
                return Err(AcmeError::Cancelled);
            }
            self.clock.sleep_seconds(effective_delay);
            delay = effective_delay.saturating_mul(2).min(max_delay);
        }
        Err(AcmeError::PollTimeout)
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
        let (max_attempts, mut delay, max_delay) = bounded_poll_policy(poll);
        for attempt in 0..max_attempts {
            if poll
                .cancellation
                .as_ref()
                .is_some_and(Dns01Cancellation::is_cancelled)
            {
                return Err(AcmeError::Cancelled);
            }
            if self.clock.now_unix_seconds() > poll.deadline_unix_seconds {
                return Err(AcmeError::PollTimeout);
            }
            let response = self.post_as_get(url)?;
            if response.status != 200 {
                return Err(problem_or_status(&response));
            }
            let order = parse_order(&response, &self.policy)?;
            match order.status.as_str() {
                "valid" | "invalid" => return Ok(order),
                "pending" | "ready" | "processing" => {}
                status => {
                    return Err(AcmeError::InvalidOrderState {
                        status: status.into(),
                    });
                }
            }
            let effective_delay = retry_after(&response, self.clock.now_unix_seconds())?
                .unwrap_or_else(|| jittered_delay(delay, max_delay, url, attempt));
            if attempt + 1 == max_attempts
                || self
                    .clock
                    .now_unix_seconds()
                    .saturating_add(effective_delay)
                    > poll.deadline_unix_seconds
            {
                return Err(AcmeError::PollTimeout);
            }
            if poll
                .cancellation
                .as_ref()
                .is_some_and(Dns01Cancellation::is_cancelled)
            {
                return Err(AcmeError::Cancelled);
            }
            self.clock.sleep_seconds(effective_delay);
            delay = effective_delay.saturating_mul(2).min(max_delay);
        }
        Err(AcmeError::PollTimeout)
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
        let mut retried_bad_nonce = false;
        loop {
            let nonce = self.take_nonce()?;
            let body = self.jws(url, nonce, payload, protected_key)?;
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
                .request(request)
                .map_err(AcmeError::Transport)?;
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
        let mut retried_bad_nonce = false;
        loop {
            let nonce = self.take_nonce()?;
            let inner = Self::jws_with_key(
                new_key,
                url,
                nonce.clone(),
                inner_payload,
                ProtectedKey::Jwk,
            )?;
            let inner = serde_json::from_slice::<Value>(&inner)
                .map_err(|_| AcmeError::MalformedResponse)?;
            let body = self.jws(url, nonce, &inner, ProtectedKey::Kid(account_url))?;
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
                .request(request)
                .map_err(AcmeError::Transport)?;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeafKeyAlgorithm {
    EcdsaP256,
    Rsa2048,
}

#[derive(Clone)]
pub struct LeafCsr {
    pub private_key_pem: SecretBytes,
    pub csr_der: Vec<u8>,
    pub csr_pem: Vec<u8>,
}

impl fmt::Debug for LeafCsr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeafCsr")
            .field("private_key", &"REDACTED")
            .field("csr_der_bytes", &self.csr_der.len())
            .field("csr_pem_bytes", &self.csr_pem.len())
            .finish_non_exhaustive()
    }
}

/// Generates a new leaf key and an exact-SAN PKCS#10 request.
///
/// # Errors
///
/// Returns an error when identifiers are unsupported or OpenSSL rejects key, SAN, or CSR creation.
pub fn generate_leaf_csr(
    identifiers: &[String],
    algorithm: LeafKeyAlgorithm,
) -> Result<LeafCsr, AcmeError> {
    let identifiers = normalize_identifiers(identifiers)?;
    let key = match algorithm {
        LeafKeyAlgorithm::EcdsaP256 => {
            let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).map_err(AcmeError::Csr)?;
            PKey::from_ec_key(EcKey::generate(&group).map_err(AcmeError::Csr)?)
                .map_err(AcmeError::Csr)?
        }
        LeafKeyAlgorithm::Rsa2048 => {
            PKey::from_rsa(Rsa::generate(2_048).map_err(AcmeError::Csr)?).map_err(AcmeError::Csr)?
        }
    };
    let private_key_pem = SecretBytes::new(key.private_key_to_pem_pkcs8().map_err(AcmeError::Csr)?);
    let mut subject = X509NameBuilder::new().map_err(AcmeError::Csr)?;
    subject
        .append_entry_by_text("commonName", &identifiers[0])
        .map_err(AcmeError::Csr)?;
    let subject = subject.build();
    let mut builder = X509ReqBuilder::new().map_err(AcmeError::Csr)?;
    builder.set_version(0).map_err(AcmeError::Csr)?;
    builder.set_subject_name(&subject).map_err(AcmeError::Csr)?;
    builder.set_pubkey(&key).map_err(AcmeError::Csr)?;
    let mut san = SubjectAlternativeName::new();
    for identifier in &identifiers {
        san.dns(identifier);
    }
    let context = builder.x509v3_context(None);
    let extension = san.build(&context).map_err(AcmeError::Csr)?;
    let mut extensions = Stack::new().map_err(AcmeError::Csr)?;
    extensions.push(extension).map_err(AcmeError::Csr)?;
    builder
        .add_extensions(&extensions)
        .map_err(AcmeError::Csr)?;
    builder
        .sign(&key, MessageDigest::sha256())
        .map_err(AcmeError::Csr)?;
    let csr: X509Req = builder.build();
    Ok(LeafCsr {
        private_key_pem,
        csr_der: csr.to_der().map_err(AcmeError::Csr)?,
        csr_pem: csr.to_pem().map_err(AcmeError::Csr)?,
    })
}

fn public_jwk(
    key: &PKey<Private>,
    algorithm: AccountKeyAlgorithm,
) -> Result<BTreeMap<String, String>, AcmeError> {
    match algorithm {
        AccountKeyAlgorithm::EcdsaP256 => {
            let ec = key.ec_key().map_err(AcmeError::Key)?;
            let mut context = BigNumContext::new().map_err(AcmeError::Key)?;
            let public = ec
                .public_key()
                .to_bytes(ec.group(), PointConversionForm::UNCOMPRESSED, &mut context)
                .map_err(AcmeError::Key)?;
            if public.len() != 65 || public[0] != 4 {
                return Err(AcmeError::MalformedResponse);
            }
            Ok([
                ("crv".into(), "P-256".into()),
                ("kty".into(), "EC".into()),
                ("x".into(), URL_SAFE_NO_PAD.encode(&public[1..33])),
                ("y".into(), URL_SAFE_NO_PAD.encode(&public[33..65])),
            ]
            .into_iter()
            .collect())
        }
        AccountKeyAlgorithm::Rsa2048 => {
            let rsa = key.rsa().map_err(AcmeError::Key)?;
            Ok([
                ("e".into(), URL_SAFE_NO_PAD.encode(rsa.e().to_vec())),
                ("kty".into(), "RSA".into()),
                ("n".into(), URL_SAFE_NO_PAD.encode(rsa.n().to_vec())),
            ]
            .into_iter()
            .collect())
        }
    }
}

fn validate_account_request(request: &AccountRequest) -> Result<(), AcmeError> {
    if !request.terms_agreed {
        return Err(AcmeError::TermsNotAgreed);
    }
    validate_account_contacts(&request.contacts)
}

fn validate_account_contacts(contacts: &[String]) -> Result<(), AcmeError> {
    if contacts.len() > 8
        || contacts.iter().any(|contact| {
            contact.is_empty()
                || contact.len() > 320
                || !contact.is_ascii()
                || !contact.starts_with("mailto:")
                || contact[7..].contains(char::is_whitespace)
        })
    {
        return Err(AcmeError::InvalidAccountRequest);
    }
    Ok(())
}

fn validate_account_status(status: &str) -> Result<(), AcmeError> {
    match status {
        "valid" => Ok(()),
        "deactivated" | "revoked" => Err(AcmeError::AccountNotUsable {
            status: status.into(),
        }),
        _ => Err(AcmeError::MalformedResponse),
    }
}

fn parse_account(
    value: &Value,
    url: String,
    policy: &OriginPolicy,
    terms_requested: bool,
) -> Result<Account, AcmeError> {
    policy.permits(&url)?;
    let status = required_string(value, "status")?;
    validate_account_status(&status)?;
    let contacts = string_array(value, "contact")?;
    validate_account_contacts(&contacts)?;
    let terms_agreed = value
        .get("termsOfServiceAgreed")
        .and_then(Value::as_bool)
        .ok_or(AcmeError::MalformedResponse)?;
    if terms_requested && !terms_agreed {
        return Err(AcmeError::TermsNotAgreed);
    }
    Ok(Account {
        url,
        status,
        contacts,
        terms_agreed,
    })
}

fn validate_order_status(status: &str) -> Result<(), AcmeError> {
    matches!(
        status,
        "pending" | "ready" | "processing" | "valid" | "invalid"
    )
    .then_some(())
    .ok_or_else(|| AcmeError::InvalidOrderState {
        status: status.into(),
    })
}

fn normalize_identifiers(identifiers: &[String]) -> Result<Vec<String>, AcmeError> {
    if identifiers.is_empty() || identifiers.len() > MAX_IDENTIFIERS {
        return Err(AcmeError::UnsupportedIdentifier);
    }
    let mut normalized = BTreeSet::new();
    for identifier in identifiers {
        let identifier = identifier.trim().to_ascii_lowercase();
        if identifier.is_empty() || identifier.len() > 253 {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        if identifier.parse::<IpAddr>().is_ok() {
            return Err(AcmeError::IpIdentifierUnsupported);
        }
        let dns_name = identifier.strip_prefix("*.").unwrap_or(&identifier);
        if identifier.contains('*') && !identifier.starts_with("*.") {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        if !valid_dns_name(dns_name) || dns_name.parse::<IpAddr>().is_ok() {
            return Err(AcmeError::UnsupportedIdentifier);
        }
        normalized.insert(identifier);
    }
    if normalized.len() != identifiers.len() {
        return Err(AcmeError::UnsupportedIdentifier);
    }
    Ok(normalized.into_iter().collect())
}

fn valid_dns_name(value: &str) -> bool {
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

fn parse_order(response: &HttpResponse, policy: &OriginPolicy) -> Result<Order, AcmeError> {
    let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
    let url = response
        .header("location")
        .map_or_else(|| response.url.clone(), str::to_owned);
    policy.permits(&url)?;
    let identifiers = value
        .get("identifiers")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|identifier| {
            match identifier.get("type").and_then(Value::as_str) {
                Some("dns") => {}
                Some("ip") => return Err(AcmeError::IpIdentifierUnsupported),
                _ => return Err(AcmeError::UnsupportedIdentifier),
            }
            identifier
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(AcmeError::MissingField)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let identifiers = normalize_identifiers(&identifiers)?;
    let authorizations = value
        .get("authorizations")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(AcmeError::MalformedResponse)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if authorizations.len() > MAX_IDENTIFIERS {
        return Err(AcmeError::MalformedResponse);
    }
    let finalize = optional_string(&value, "finalize")?;
    let certificate = optional_string(&value, "certificate")?;
    for endpoint in authorizations
        .iter()
        .chain(finalize.iter())
        .chain(certificate.iter())
    {
        policy.permits(endpoint)?;
    }
    let status = required_string(&value, "status")?;
    validate_order_status(&status)?;
    Ok(Order {
        url,
        status,
        identifiers,
        authorizations,
        finalize,
        certificate,
    })
}

#[allow(clippy::too_many_lines)]
fn parse_authorization(
    response: &HttpResponse,
    url: &str,
    key: &AccountKey,
    policy: &OriginPolicy,
    challenge_type: ChallengeType,
) -> Result<Authorization, AcmeError> {
    let value = bounded_json(&response.body, MAX_ACME_BODY_BYTES)?;
    let identifier = value
        .get("identifier")
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .ok_or(AcmeError::MissingField)?
        .to_ascii_lowercase();
    normalize_identifiers(std::slice::from_ref(&identifier))?;
    let status = AuthorizationStatus::parse(&required_string(&value, "status")?)?;
    let challenges = value
        .get("challenges")
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?;
    let http01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("http-01"));
    let challenge = http01_challenge
        .map(|challenge| {
            let challenge_url = challenge
                .get("url")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            policy.permits(&challenge_url)?;
            let token = challenge
                .get("token")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            validate_token(&token)?;
            Ok(Http01Challenge {
                authorization_url: url.into(),
                challenge_url,
                token: token.clone(),
                key_authorization: key.key_authorization(&token),
            })
        })
        .transpose()?;
    let dns01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("dns-01"))
        .map(|challenge| {
            let challenge_url = challenge
                .get("url")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            policy.permits(&challenge_url)?;
            let token = challenge
                .get("token")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            validate_token(&token)?;
            let dns_name = identifier.strip_prefix("*.").unwrap_or(&identifier);
            let record_name = format!("_acme-challenge.{dns_name}");
            let key_authorization = key.key_authorization(&token);
            let record_value = URL_SAFE_NO_PAD.encode(Sha256::digest(key_authorization.as_bytes()));
            Dns01Challenge::new(&identifier, challenge_url, record_name, record_value)
                .map_err(|_| AcmeError::MalformedResponse)
        })
        .transpose()?;
    let tls_alpn01_challenge = challenges
        .iter()
        .find(|challenge| challenge.get("type").and_then(Value::as_str) == Some("tls-alpn-01"))
        .map(|challenge| {
            let challenge_url = challenge
                .get("url")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            policy.permits(&challenge_url)?;
            let token = challenge
                .get("token")
                .and_then(Value::as_str)
                .ok_or(AcmeError::MissingField)?
                .to_owned();
            validate_token(&token)?;
            Ok(TlsAlpn01Challenge {
                authorization_url: url.into(),
                challenge_url,
                token: token.clone(),
                key_authorization: key.key_authorization(&token),
            })
        })
        .transpose()?;
    let selected = match challenge_type {
        ChallengeType::Http01 => challenge.is_some(),
        ChallengeType::Dns01 => dns01_challenge.is_some(),
        ChallengeType::TlsAlpn01 => tls_alpn01_challenge.is_some(),
    };
    if !selected
        && matches!(
            status,
            AuthorizationStatus::Pending | AuthorizationStatus::Processing
        )
    {
        return Err(AcmeError::UnsupportedChallenge);
    }
    Ok(Authorization {
        url: url.into(),
        identifier,
        status,
        challenge,
        dns01_challenge,
        tls_alpn01_challenge,
    })
}

fn validate_token(token: &str) -> Result<(), AcmeError> {
    if token.is_empty()
        || token.len() > 256
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AcmeError::MalformedResponse);
    }
    Ok(())
}

fn renewal_information_endpoint(base: &str, certificate_id: &str) -> Result<String, AcmeError> {
    if base.contains(['?', '#']) {
        return Err(AcmeError::InvalidRenewalInformation);
    }
    let separator = if base.ends_with('/') { "" } else { "/" };
    let endpoint = format!("{base}{separator}{certificate_id}");
    (endpoint.len() <= MAX_ACME_URL_BYTES)
        .then_some(endpoint)
        .ok_or(AcmeError::InvalidRenewalInformation)
}

fn parse_renewal_timestamp(value: &str) -> Result<u64, AcmeError> {
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return Err(AcmeError::InvalidRenewalInformation);
    }
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| AcmeError::InvalidRenewalInformation)?
        .unix_timestamp();
    u64::try_from(timestamp).map_err(|_| AcmeError::InvalidRenewalInformation)
}

fn validate_nonce(nonce: &str) -> Result<(), AcmeError> {
    if nonce.is_empty()
        || nonce.len() > 256
        || !nonce.is_ascii()
        || nonce.bytes().any(|byte| byte <= b' ')
    {
        return Err(AcmeError::MalformedResponse);
    }
    Ok(())
}

fn bounded_json(bytes: &[u8], limit: usize) -> Result<Value, AcmeError> {
    if bytes.len() > limit {
        return Err(AcmeError::ResponseTooLarge { limit });
    }
    serde_json::from_slice(bytes).map_err(|_| AcmeError::MalformedResponse)
}

fn required_string(value: &Value, name: &str) -> Result<String, AcmeError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_ACME_URL_BYTES)
        .map(str::to_owned)
        .ok_or(AcmeError::MissingField)
}

fn optional_string(value: &Value, name: &str) -> Result<Option<String>, AcmeError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= MAX_ACME_URL_BYTES)
            .map(str::to_owned)
            .map(Some)
            .ok_or(AcmeError::MalformedResponse),
    }
}

fn string_array(value: &Value, name: &str) -> Result<Vec<String>, AcmeError> {
    value
        .get(name)
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(AcmeError::MalformedResponse)
        })
        .collect()
}

fn problem_or_status(response: &HttpResponse) -> AcmeError {
    parse_json_body(&response.body)
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .map_or(
            AcmeError::UnexpectedStatus {
                status: response.status,
            },
            |problem_type| AcmeError::Problem { problem_type },
        )
}

fn is_bad_nonce(response: &HttpResponse) -> bool {
    response.status == 400
        && parse_json_body(&response.body)
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|problem_type| problem_type.ends_with("badNonce"))
}

fn parse_json_body(body: &[u8]) -> Option<Value> {
    (body.len() <= MAX_ACME_BODY_BYTES)
        .then(|| serde_json::from_slice::<Value>(body).ok())
        .flatten()
}

fn bounded_poll_policy(poll: &PollPolicy) -> (usize, u64, u64) {
    let max_attempts = poll.max_attempts.min(MAX_POLL_ATTEMPTS);
    let max_delay = poll.max_delay_seconds.min(MAX_POLL_DELAY_SECONDS);
    let initial_delay = poll.initial_delay_seconds.min(max_delay);
    (max_attempts, initial_delay, max_delay)
}

fn jittered_delay(base: u64, max_delay: u64, identity: &str, attempt: usize) -> u64 {
    if base == 0 || max_delay == 0 {
        return 0;
    }
    let digest = Sha256::digest(format!("{identity}:{attempt}").as_bytes());
    let jitter_limit = base.min(5);
    let jitter = u64::from(digest[0]) % (jitter_limit + 1);
    base.saturating_add(jitter).min(max_delay)
}

fn retry_after(response: &HttpResponse, now_unix_seconds: u64) -> Result<Option<u64>, AcmeError> {
    let Some(value) = response.header("retry-after") else {
        return Ok(None);
    };
    let seconds = if let Ok(seconds) = value.parse::<u64>() {
        seconds
    } else {
        let date = httpdate::parse_http_date(value).map_err(|_| AcmeError::InvalidRetryAfter)?;
        date.duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AcmeError::InvalidRetryAfter)?
            .as_secs()
            .saturating_sub(now_unix_seconds)
    };
    if seconds > MAX_POLL_DELAY_SECONDS {
        return Err(AcmeError::InvalidRetryAfter);
    }
    Ok(Some(seconds))
}

fn validate_response_url(
    requested_url: &str,
    response: &HttpResponse,
    policy: &OriginPolicy,
) -> Result<(), AcmeError> {
    if response.url.is_empty() {
        return Err(AcmeError::MalformedResponse);
    }
    if origin_of(requested_url)? != origin_of(&response.url)? {
        return Err(AcmeError::UntrustedRedirect);
    }
    policy.permits(requested_url)?;
    policy.permits(&response.url)?;
    Ok(())
}

fn origin_of(url: &str) -> Result<String, AcmeError> {
    if url.len() > MAX_ACME_URL_BYTES || !url.starts_with("https://") {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    let rest = &url[8..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.bytes().any(|byte| byte <= b' ')
    {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(AcmeError::InvalidDirectoryUrl)?;
        let port = authority[end + 1..].strip_prefix(':');
        if end + 1 < authority.len() && port.is_none() {
            return Err(AcmeError::InvalidDirectoryUrl);
        }
        (&authority[1..end], port)
    } else {
        if authority.matches(':').count() > 1 {
            return Err(AcmeError::InvalidDirectoryUrl);
        }
        authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)))
    };
    if host.is_empty() || host.len() > 253 || host.eq_ignore_ascii_case("localhost") {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    if let Some(port) = port
        && (port.is_empty() || port.parse::<u16>().is_err())
    {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    Ok(format!("https://{}", authority.to_ascii_lowercase()))
}

fn is_private_origin(origin: &str) -> bool {
    let authority = origin.strip_prefix("https://").unwrap_or_default();
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .or_else(|| authority.split(':').next())
        .unwrap_or_default();
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_loopback()
                || address.is_private()
                || address.is_link_local()
                || address.is_unspecified()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
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
            br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#.to_vec(),
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
    fn revocation_rejects_unknown_reason_codes() {
        let transport =
            ScriptedTransport::new([directory_with_actions("https://acme.test/directory")]);
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
    fn renewal_information_uses_the_leaf_hash_and_bounded_window() {
        let certificate = renewal_test_certificate();
        let transport = ScriptedTransport::new([
            directory_with_renewal_info("https://acme.test/directory"),
            HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
                .with_header("replay-nonce", "nonce-ari"),
            HttpResponse::new(
                200,
                "https://acme.test/acme/renewal-info",
                br#"{"suggestedWindow":{"start":"2026-08-10T00:00:00Z","end":"2026-08-20T00:00:00Z"}}"#.to_vec(),
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
                br#"{"suggestedWindow":{"start":"2026-01-01T00:00:00Z","end":"2028-01-01T00:00:00Z"}}"#.to_vec(),
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
        let mut client =
            AcmeClient::new(transport, "https://acme.test/directory", policy, key, clock)
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
}
