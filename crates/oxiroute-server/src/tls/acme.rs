use std::{
    io,
    sync::{Arc, Mutex, TryLockError},
    time::{SystemTime, UNIX_EPOCH},
};

use openssl::{
    asn1::{Asn1Time, Asn1TimeRef},
    x509::X509,
};
use oxiroute_acme::{
    Account, AccountKey, AccountKeyAlgorithm, AcmeClient, AcmeError, AcmeStateError, AcmeTransport,
    AuthorizationStatus, CertificateMaterial, ChallengeRecord, ChallengeStore, ChallengeStoreError,
    JobState, JobStatus, LeafKeyAlgorithm, OriginPolicy, PollPolicy, RedactedOutcome,
    RevisionMetadata, RevisionStore, SecretBytes, StateStore, SystemAcmeTransport, SystemClock,
    renewal_due, stable_renewal_time,
};
use oxiroute_config::{AcmeKeyType, SelfSignedKeyType};
use serde::{Deserialize, Serialize};

use super::{ActiveCertificateGeneration, CertificateGeneration, TlsBuildError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcmeManagedOutcome {
    Loaded,
    Unchanged,
    Activated,
}

#[derive(Clone, Debug)]
pub struct AcmeManagedPolicy {
    pub directory_url: String,
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
    pub key_type: AcmeKeyType,
    pub allowed_dns_suffixes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AcmeManagedError {
    #[error("managed ACME job is already running")]
    Busy,
    #[error("managed ACME state is unavailable")]
    State(#[source] AcmeStateError),
    #[error("managed ACME protocol operation failed")]
    Protocol(#[source] AcmeError),
    #[error("managed ACME TLS candidate was rejected")]
    Tls(#[source] Box<TlsBuildError>),
    #[error("managed ACME authorization did not become valid")]
    AuthorizationFailed,
    #[error("managed ACME order did not become valid")]
    OrderFailed,
    #[error("managed ACME certificate response is malformed")]
    CertificateMalformed,
    #[error("managed ACME account belongs to a different configured directory")]
    AccountDirectoryChanged,
    #[error("managed ACME publication lost its generation race")]
    Publication,
    #[error("managed ACME challenge could not be provisioned")]
    Challenge(#[source] ChallengeStoreError),
}

impl AcmeManagedError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Busy => "job_busy",
            Self::State(_) => "state_failed",
            Self::Protocol(error) => match error {
                AcmeError::PollTimeout => "poll_timeout",
                AcmeError::Transport(_) => "transport_failed",
                AcmeError::Problem { .. } => "acme_problem",
                _ => "protocol_failed",
            },
            Self::Tls(_) => "invalid_candidate",
            Self::AuthorizationFailed => "authorization_failed",
            Self::OrderFailed => "order_failed",
            Self::CertificateMalformed => "certificate_malformed",
            Self::AccountDirectoryChanged => "account_directory_changed",
            Self::Publication => "publication_conflict",
            Self::Challenge(_) => "challenge_failed",
        }
    }
}

impl AcmeManagedOutcome {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Unchanged => "unchanged",
            Self::Activated => "activated",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcmeManagedStatus {
    pub certificate: String,
    pub directory_url: String,
    pub key_type: String,
    pub allowed_dns_suffixes: Vec<String>,
    pub disk_revision: String,
    pub active_revision: String,
    pub not_before_unix_seconds: Option<u64>,
    pub not_after_unix_seconds: Option<u64>,
    pub next_action_unix_seconds: Option<u64>,
    pub not_after: String,
    pub last_outcome: Option<&'static str>,
    pub last_error_code: Option<&'static str>,
}

struct ReconcileState {
    disk_revision: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    last_outcome: Option<&'static str>,
    last_error_code: Option<&'static str>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedAccount {
    directory_url: String,
    account: Account,
}

/// Owns the managed state root for one certificate and publishes only validated revisions.
pub struct AcmeManagedReconciler {
    certificate: String,
    declared_dns_names: Vec<String>,
    policy: AcmeManagedPolicy,
    revisions: RevisionStore,
    challenge_store: ChallengeStore,
    active: Arc<ActiveCertificateGeneration>,
    job: Mutex<()>,
    state: Mutex<ReconcileState>,
}

type LoadedManagedCertificate = (
    CertificateGeneration,
    RevisionStore,
    String,
    Option<u64>,
    Option<u64>,
    bool,
);

impl AcmeManagedReconciler {
    /// Opens and validates the current managed revision before constructing the runtime slot.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed current pointer, certificate material, or TLS validation
    /// cannot be read or accepted.
    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn load(
        certificate: impl Into<String>,
        declared_dns_names: &[String],
        state: Arc<StateStore>,
        key_type: AcmeKeyType,
    ) -> Result<LoadedManagedCertificate, TlsBuildError> {
        let certificate = certificate.into();
        let revisions = RevisionStore::from_arc(state);
        let material = match revisions.load_current(&certificate) {
            Ok(material) => material,
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let generation = CertificateGeneration::self_signed_development(
                    certificate,
                    declared_dns_names,
                    1,
                    bootstrap_key_type(key_type),
                )?;
                return Ok((generation, revisions, "bootstrap".into(), None, None, true));
            }
            Err(source) => {
                return Err(TlsBuildError::AcmeState {
                    certificate: certificate.clone(),
                    source: Box::new(source),
                });
            }
        };
        let current = revisions
            .state()
            .root()
            .join(format!("certificates/{certificate}/current"));
        let generation = CertificateGeneration::from_pem(
            certificate.clone(),
            declared_dns_names,
            &current.join("fullchain.pem"),
            &material.fullchain_pem,
            &current.join("privkey.pem"),
            material.private_key_pem.as_bytes(),
        )?;
        Ok((
            generation,
            revisions,
            material.metadata.revision,
            material.metadata.not_before_unix_seconds,
            material.metadata.not_after_unix_seconds,
            false,
        ))
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        let certificate = certificate.into();
        let next_action_unix_seconds = if initial_issuance_due {
            Some(0)
        } else {
            not_before_unix_seconds.and_then(|not_before| {
                not_after_unix_seconds
                    .and_then(|not_after| stable_renewal_time(not_before, not_after, &certificate))
            })
        };
        Self {
            certificate,
            declared_dns_names,
            policy,
            revisions,
            challenge_store,
            active,
            job: Mutex::new(()),
            state: Mutex::new(ReconcileState {
                disk_revision,
                not_before_unix_seconds,
                not_after_unix_seconds,
                next_action_unix_seconds,
                last_outcome: Some(AcmeManagedOutcome::Loaded.code()),
                last_error_code: None,
            }),
        }
    }

    #[must_use]
    pub const fn active_generation(&self) -> &Arc<ActiveCertificateGeneration> {
        &self.active
    }

    #[must_use]
    pub const fn revisions(&self) -> &RevisionStore {
        &self.revisions
    }

    #[must_use]
    pub fn status(&self) -> AcmeManagedStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = self.active.snapshot();
        AcmeManagedStatus {
            certificate: self.certificate.clone(),
            directory_url: self.policy.directory_url.clone(),
            key_type: match self.policy.key_type {
                AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                AcmeKeyType::Rsa2048 => "rsa_2048",
            }
            .into(),
            allowed_dns_suffixes: self.policy.allowed_dns_suffixes.clone(),
            disk_revision: state.disk_revision.clone(),
            active_revision: generation.metadata().revision.clone(),
            not_before_unix_seconds: state.not_before_unix_seconds,
            not_after_unix_seconds: state.not_after_unix_seconds,
            next_action_unix_seconds: state.next_action_unix_seconds,
            not_after: generation.metadata().validity.not_after.clone(),
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code,
        }
    }

    #[must_use]
    pub fn renewal_due(&self, now_unix_seconds: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .next_action_unix_seconds
            .is_some_and(|next| now_unix_seconds >= next)
        {
            return true;
        }
        match (state.not_before_unix_seconds, state.not_after_unix_seconds) {
            (Some(not_before), Some(not_after)) => {
                renewal_due(now_unix_seconds, not_before, not_after, None)
            }
            _ => false,
        }
    }

    /// Reloads the atomically selected revision and publishes it only if it remains the same
    /// certificate identity and DNS set.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected revision is unavailable, invalid, or loses a publication
    /// race with another writer.
    pub fn reconcile(&self) -> Result<AcmeManagedOutcome, TlsBuildError> {
        let _job = self
            .job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let material = self
            .revisions
            .load_current(&self.certificate)
            .map_err(|source| self.state_error(source))?;
        let root = self.revisions.state().root();
        let current = root.join(format!("certificates/{}/current", self.certificate));
        let candidate = CertificateGeneration::from_pem(
            self.certificate.clone(),
            &self.declared_dns_names,
            &current.join("fullchain.pem"),
            &material.fullchain_pem,
            &current.join("privkey.pem"),
            material.private_key_pem.as_bytes(),
        )
        .inspect_err(|_| {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.last_outcome = None;
            state.last_error_code = Some("invalid_candidate");
        })?;
        let expected = self.active.snapshot();
        let disk_revision = material.metadata.revision.clone();
        let outcome = if candidate.metadata().revision == expected.metadata().revision {
            AcmeManagedOutcome::Unchanged
        } else {
            self.active
                .publish_if_current(&expected, Arc::new(candidate))
                .map_err(|error| TlsBuildError::AcmePublication {
                    certificate: self.certificate.clone(),
                    source: error,
                })?;
            AcmeManagedOutcome::Activated
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = disk_revision;
        state.last_outcome = Some(outcome.code());
        state.last_error_code = None;
        Ok(outcome)
    }

    /// Runs one bounded managed issuance or renewal using the production HTTPS transport.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_now(&self) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_with_transport(SystemAcmeTransport::default())
    }

    /// Runs one bounded managed issuance or renewal with an injected transport.
    ///
    /// The injected form is used by deterministic local ACME tests and never changes the
    /// production transport used by [`Self::renew_now`].
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_with_transport<T: AcmeTransport>(
        &self,
        transport: T,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        let _job = match self.job.try_lock() {
            Ok(job) => job,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(AcmeManagedError::Busy),
        };
        let now = unix_now();
        let job_id = format!("renew-{now}");
        self.write_job(&job_id, JobStatus::Queued, now, 0, None, None, None, None)
            .map_err(AcmeManagedError::State)?;
        let result = self.renew_inner(transport, &job_id, now);
        match &result {
            Ok(outcome) => {
                let status = self.status();
                self.set_outcome(Some(outcome.code()), None);
                self.write_job(
                    &job_id,
                    JobStatus::Succeeded,
                    unix_now(),
                    1,
                    None,
                    Some(status.disk_revision.clone()),
                    Some(status.active_revision.clone()),
                    Some(RedactedOutcome::new(
                        outcome.code(),
                        "managed ACME renewal completed",
                    )),
                )
                .map_err(AcmeManagedError::State)?;
            }
            Err(error) => {
                self.set_outcome(None, Some(error.code()));
                let status = self.status();
                let _ = self.write_job(
                    &job_id,
                    JobStatus::Failed,
                    unix_now(),
                    1,
                    None,
                    Some(status.disk_revision),
                    Some(status.active_revision),
                    Some(RedactedOutcome::new(
                        error.code(),
                        "managed ACME renewal failed",
                    )),
                );
            }
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    fn renew_inner<T: AcmeTransport>(
        &self,
        transport: T,
        job_id: &str,
        created_at: u64,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.write_job(
            job_id,
            JobStatus::Running,
            unix_now(),
            1,
            None,
            None,
            None,
            None,
        )
        .map_err(AcmeManagedError::State)?;
        let key_algorithm = account_key_algorithm(self.policy.key_type);
        let account_key_path = format!("accounts/{}/account-key.pem", self.certificate);
        let account_key = match self
            .revisions
            .state()
            .read_bounded(&account_key_path, oxiroute_acme::MAX_CERTIFICATE_BYTES)
        {
            Ok(bytes) => {
                AccountKey::from_pem(SecretBytes::new(bytes)).map_err(AcmeManagedError::Protocol)?
            }
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let key =
                    AccountKey::generate(key_algorithm).map_err(AcmeManagedError::Protocol)?;
                self.revisions
                    .state()
                    .write_secret(&account_key_path, key.private_key_pem())
                    .map_err(AcmeManagedError::State)?;
                key
            }
            Err(error) => return Err(AcmeManagedError::State(error)),
        };
        let origin =
            OriginPolicy::strict(&self.policy.directory_url).map_err(AcmeManagedError::Protocol)?;
        let mut client = AcmeClient::new(
            transport,
            &self.policy.directory_url,
            origin,
            account_key,
            Arc::new(SystemClock),
        )
        .map_err(AcmeManagedError::Protocol)?;
        let account_path = format!("accounts/{}/account.json", self.certificate);
        match self
            .revisions
            .state()
            .read_json::<PersistedAccount>(&account_path, oxiroute_acme::MAX_JOB_BYTES)
        {
            Ok(persisted) => {
                if persisted.directory_url != self.policy.directory_url {
                    return Err(AcmeManagedError::AccountDirectoryChanged);
                }
                client
                    .set_account(persisted.account)
                    .map_err(AcmeManagedError::Protocol)?;
            }
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let account = client
                    .register_account(&oxiroute_acme::AccountRequest {
                        contacts: self.policy.contacts.clone(),
                        terms_agreed: self.policy.terms_agreed,
                    })
                    .map_err(AcmeManagedError::Protocol)?;
                self.revisions
                    .state()
                    .write_json(
                        &account_path,
                        &PersistedAccount {
                            directory_url: self.policy.directory_url.clone(),
                            account,
                        },
                    )
                    .map_err(AcmeManagedError::State)?;
            }
            Err(error) => return Err(AcmeManagedError::State(error)),
        }
        let order = client
            .create_order(&oxiroute_acme::CertificateRequest {
                identifiers: self.declared_dns_names.clone(),
            })
            .map_err(AcmeManagedError::Protocol)?;
        for (index, authorization_url) in order.authorizations.iter().enumerate() {
            let authorization = client
                .authorization(authorization_url)
                .map_err(AcmeManagedError::Protocol)?;
            if authorization.status == AuthorizationStatus::Valid {
                continue;
            }
            if !matches!(
                authorization.status,
                AuthorizationStatus::Pending | AuthorizationStatus::Processing
            ) {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
            let challenge = authorization
                .challenge
                .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
            let lease = self
                .challenge_store
                .provision(ChallengeRecord {
                    token: challenge.token.clone(),
                    key_authorization: challenge.key_authorization.clone(),
                    account_id: "managed-account".into(),
                    order_id: job_id.into(),
                    authorization_id: format!("authorization-{index}"),
                    challenge_id: format!("challenge-{index}"),
                    created_at_unix_seconds: created_at,
                    expires_at_unix_seconds: created_at.saturating_add(600),
                })
                .map_err(AcmeManagedError::Challenge)?;
            client
                .respond_to_challenge(&challenge)
                .map_err(AcmeManagedError::Protocol)?;
            let authorization = client
                .poll_authorization(
                    &authorization.url,
                    &poll_policy(unix_now().saturating_add(600)),
                )
                .map_err(AcmeManagedError::Protocol)?;
            lease.complete();
            if authorization.status != AuthorizationStatus::Valid {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
        }
        let csr = oxiroute_acme::generate_leaf_csr(
            &self.declared_dns_names,
            leaf_key_algorithm(self.policy.key_type),
        )
        .map_err(AcmeManagedError::Protocol)?;
        let finalized = client
            .finalize_order(&order, &csr.csr_der)
            .map_err(AcmeManagedError::Protocol)?;
        let valid_order = client
            .poll_order(&finalized.url, &poll_policy(unix_now().saturating_add(600)))
            .map_err(AcmeManagedError::Protocol)?;
        if valid_order.status != "valid" {
            return Err(AcmeManagedError::OrderFailed);
        }
        let certificate_url = valid_order
            .certificate
            .ok_or(AcmeManagedError::Protocol(AcmeError::MissingField))?;
        let certificate_pem = client
            .download_certificate(&certificate_url)
            .map_err(AcmeManagedError::Protocol)?;
        let (mut material, candidate) = self.certificate_material(&csr, &certificate_pem)?;
        material
            .metadata
            .revision
            .clone_from(&candidate.metadata().revision);
        let revision = material.metadata.revision.clone();
        self.revisions
            .commit(&self.certificate, &revision, &material)
            .map_err(AcmeManagedError::State)?;
        let expected = self.active.snapshot();
        let outcome = if candidate.metadata().revision == expected.metadata().revision {
            AcmeManagedOutcome::Unchanged
        } else {
            self.active
                .publish_if_current(&expected, Arc::new(candidate))
                .map_err(|_| AcmeManagedError::Publication)?;
            AcmeManagedOutcome::Activated
        };
        self.update_schedule(
            material.metadata.not_before_unix_seconds,
            material.metadata.not_after_unix_seconds,
            revision,
            outcome.code(),
        );
        Ok(outcome)
    }

    fn certificate_material(
        &self,
        csr: &oxiroute_acme::LeafCsr,
        certificate_pem: &[u8],
    ) -> Result<(CertificateMaterial, CertificateGeneration), AcmeManagedError> {
        let certificates = X509::stack_from_pem(certificate_pem)
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        if certificates.len() < 2 {
            return Err(AcmeManagedError::CertificateMalformed);
        }
        let mut fullchain_pem = Vec::new();
        let mut chain_pem = Vec::new();
        for (index, certificate) in certificates.iter().enumerate() {
            let pem = certificate
                .to_pem()
                .map_err(|_| AcmeManagedError::CertificateMalformed)?;
            fullchain_pem.extend_from_slice(&pem);
            if index > 0 {
                chain_pem.extend_from_slice(&pem);
            }
        }
        let certificate_pem = certificates[0]
            .to_pem()
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        let mut material = CertificateMaterial {
            certificate_pem,
            chain_pem,
            fullchain_pem,
            private_key_pem: csr.private_key_pem.clone(),
            metadata: RevisionMetadata {
                certificate: self.certificate.clone(),
                revision: "pending".into(),
                created_at_unix_seconds: unix_now(),
                not_before_unix_seconds: asn1_unix(certificates[0].not_before()),
                not_after_unix_seconds: asn1_unix(certificates[0].not_after()),
                issuer: None,
                serial_fingerprint: None,
                key_type: Some(
                    match self.policy.key_type {
                        AcmeKeyType::EcdsaP256 => "ecdsa_p256",
                        AcmeKeyType::Rsa2048 => "rsa_2048",
                    }
                    .into(),
                ),
            },
        };
        let root = self.revisions.state().root();
        let current = root.join(format!("certificates/{}/current", self.certificate));
        let candidate = CertificateGeneration::from_pem(
            self.certificate.clone(),
            &self.declared_dns_names,
            &current.join("fullchain.pem"),
            &material.fullchain_pem,
            &current.join("privkey.pem"),
            material.private_key_pem.as_bytes(),
        )
        .map_err(|source| AcmeManagedError::Tls(Box::new(source)))?;
        material
            .metadata
            .revision
            .clone_from(&candidate.metadata().revision);
        Ok((material, candidate))
    }

    #[allow(clippy::too_many_arguments)]
    fn write_job(
        &self,
        id: &str,
        status: JobStatus,
        now: u64,
        attempt: u32,
        next_action_unix_seconds: Option<u64>,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        last_outcome: Option<RedactedOutcome>,
    ) -> Result<(), AcmeStateError> {
        self.revisions.state().write_job(&JobState {
            id: id.into(),
            certificate: self.certificate.clone(),
            operation: "renew".into(),
            status,
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
            attempt,
            next_action_unix_seconds,
            disk_revision,
            active_revision,
            last_outcome,
        })
    }

    fn set_outcome(&self, outcome: Option<&'static str>, error: Option<&'static str>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.last_outcome = outcome;
        state.last_error_code = error;
    }

    fn update_schedule(
        &self,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        disk_revision: String,
        outcome: &'static str,
    ) {
        let next_action_unix_seconds = not_before_unix_seconds.and_then(|not_before| {
            not_after_unix_seconds
                .and_then(|not_after| stable_renewal_time(not_before, not_after, &self.certificate))
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disk_revision = disk_revision;
        state.not_before_unix_seconds = not_before_unix_seconds;
        state.not_after_unix_seconds = not_after_unix_seconds;
        state.next_action_unix_seconds = next_action_unix_seconds;
        state.last_outcome = Some(outcome);
        state.last_error_code = None;
    }

    fn state_error(&self, source: AcmeStateError) -> TlsBuildError {
        TlsBuildError::AcmeState {
            certificate: self.certificate.clone(),
            source: Box::new(source),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn account_key_algorithm(key_type: AcmeKeyType) -> AccountKeyAlgorithm {
    match key_type {
        AcmeKeyType::EcdsaP256 => AccountKeyAlgorithm::EcdsaP256,
        AcmeKeyType::Rsa2048 => AccountKeyAlgorithm::Rsa2048,
    }
}

fn leaf_key_algorithm(key_type: AcmeKeyType) -> LeafKeyAlgorithm {
    match key_type {
        AcmeKeyType::EcdsaP256 => LeafKeyAlgorithm::EcdsaP256,
        AcmeKeyType::Rsa2048 => LeafKeyAlgorithm::Rsa2048,
    }
}

fn bootstrap_key_type(key_type: AcmeKeyType) -> SelfSignedKeyType {
    match key_type {
        AcmeKeyType::EcdsaP256 => SelfSignedKeyType::EcdsaP256,
        AcmeKeyType::Rsa2048 => SelfSignedKeyType::Rsa2048,
    }
}

fn poll_policy(deadline_unix_seconds: u64) -> PollPolicy {
    PollPolicy {
        max_attempts: 64,
        deadline_unix_seconds,
        initial_delay_seconds: 1,
        max_delay_seconds: 60,
    }
}

fn asn1_unix(value: &Asn1TimeRef) -> Option<u64> {
    let epoch = Asn1Time::from_unix(0).ok()?;
    let difference = epoch.diff(value).ok()?;
    if difference.days < 0 || difference.secs < 0 {
        return None;
    }
    u64::try_from(difference.days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(u64::try_from(difference.secs).ok()?)
}

impl std::fmt::Debug for AcmeManagedReconciler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcmeManagedReconciler")
            .field("certificate", &self.certificate)
            .field("state_root", &self.revisions.state().root())
            .field(
                "active_revision",
                &self.active.snapshot().metadata().revision,
            )
            .finish_non_exhaustive()
    }
}
