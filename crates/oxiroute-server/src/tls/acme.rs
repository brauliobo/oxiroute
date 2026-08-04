use std::{
    collections::BTreeSet,
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, TryLockError,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use openssl::{
    asn1::{Asn1Time, Asn1TimeRef},
    x509::X509,
};
use oxiroute_acme::{
    renewal_due, stable_renewal_time, Account, AccountKey, AccountKeyAlgorithm, AcmeClient,
    AcmeError, AcmeStateError, AcmeTransport, AuthorizationStatus, CertificateMaterial,
    ChallengeRecord, ChallengeStore, ChallengeStoreError, JobState, JobStatus, LeafKeyAlgorithm,
    OriginPolicy, PollPolicy, RedactedOutcome, RevisionMetadata, RevisionStore, SecretBytes,
    StateStore, SystemAcmeTransport, SystemClock, MAX_JOB_BYTES,
};
use oxiroute_config::{AcmeKeyType, SelfSignedKeyType};
use serde::{Deserialize, Serialize};

use super::{
    ActiveCertificateGeneration, CertificateGeneration, TlsBuildError, MAX_CERTIFICATE_CHAIN_BYTES,
};

static NEXT_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const ACME_RETRY_BASE_SECONDS: u64 = 5 * 60;
const ACME_RETRY_MAX_SECONDS: u64 = 12 * 60 * 60;

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
    pub job_status: Option<JobStatus>,
    pub retry_attempt: u32,
    pub last_success_unix_seconds: Option<u64>,
    pub last_outcome: Option<&'static str>,
    pub last_error_code: Option<String>,
}

struct ReconcileState {
    disk_revision: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    retry_attempt: u32,
    retry_at_unix_seconds: Option<u64>,
    suggested_renewal_unix_seconds: Option<u64>,
    account_url: Option<String>,
    renewal_info_url: Option<String>,
    job_status: Option<JobStatus>,
    last_success_unix_seconds: Option<u64>,
    last_outcome: Option<&'static str>,
    last_error_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedAccount {
    directory_url: String,
    account: Account,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedRenewal {
    certificate: String,
    identifiers: Vec<String>,
    directory_url: String,
    account_url: Option<String>,
    authenticator: String,
    key_type: String,
    next_action_unix_seconds: Option<u64>,
    retry_at_unix_seconds: Option<u64>,
    retry_attempt: u32,
    suggested_renewal_unix_seconds: Option<u64>,
    renewal_info_url: Option<String>,
    last_success_unix_seconds: Option<u64>,
    #[serde(default)]
    last_error_code: Option<String>,
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
        let persisted = if initial_issuance_due {
            None
        } else {
            read_persisted_renewal(&revisions, &certificate).filter(|renewal| {
                renewal.certificate == certificate
                    && renewal.identifiers == declared_dns_names
                    && renewal.directory_url == policy.directory_url
                    && renewal.authenticator == "http01"
                    && renewal.key_type == key_type_string(policy.key_type)
            })
        };
        let next_action_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.next_action_unix_seconds)
            .or_else(|| {
                if initial_issuance_due {
                    Some(0)
                } else {
                    not_before_unix_seconds.and_then(|not_before| {
                        not_after_unix_seconds.and_then(|not_after| {
                            stable_renewal_time(not_before, not_after, &certificate)
                        })
                    })
                }
            });
        let retry_attempt = persisted
            .as_ref()
            .map_or(0, |renewal| renewal.retry_attempt);
        let retry_at_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.retry_at_unix_seconds);
        let suggested_renewal_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.suggested_renewal_unix_seconds);
        let account_url = persisted
            .as_ref()
            .and_then(|renewal| renewal.account_url.clone());
        let renewal_info_url = persisted
            .as_ref()
            .and_then(|renewal| renewal.renewal_info_url.clone());
        let last_success_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_success_unix_seconds);
        let last_error_code = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_error_code.as_deref())
            .filter(|code| is_known_error_code(code))
            .map(str::to_owned);
        let last_outcome = Some(AcmeManagedOutcome::Loaded.code());
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
                retry_attempt,
                retry_at_unix_seconds,
                suggested_renewal_unix_seconds,
                account_url,
                renewal_info_url,
                job_status: None,
                last_success_unix_seconds,
                last_outcome,
                last_error_code,
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
            job_status: state.job_status.clone(),
            retry_attempt: state.retry_attempt,
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code.clone(),
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
            (Some(not_before), Some(not_after)) => renewal_due(
                now_unix_seconds,
                not_before,
                not_after,
                state.suggested_renewal_unix_seconds,
            ),
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
            state.last_error_code = Some("invalid_candidate".into());
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
        let sequence = NEXT_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("renew-{now}-{sequence}");
        self.set_job_status(Some(JobStatus::Queued));
        self.write_job(&job_id, JobStatus::Queued, now, 0, None, None, None, None)
            .map_err(AcmeManagedError::State)?;
        let result = self.renew_inner(transport, &job_id, now);
        match &result {
            Ok(outcome) => {
                let status = self.status();
                self.set_outcome(Some(outcome.code()), None);
                self.set_job_status(Some(JobStatus::Succeeded));
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
                let retry_error = self.schedule_retry(unix_now()).err();
                if retry_error.is_some() {
                    self.set_outcome(None, Some("state_failed"));
                }
                self.set_job_status(Some(JobStatus::Failed));
                let status = self.status();
                let outcome_code = if retry_error.is_some() {
                    "state_failed"
                } else {
                    error.code()
                };
                let job_error = self
                    .write_job(
                        &job_id,
                        JobStatus::Failed,
                        unix_now(),
                        1,
                        None,
                        Some(status.disk_revision),
                        Some(status.active_revision),
                        Some(RedactedOutcome::new(
                            outcome_code,
                            "managed ACME renewal failed",
                        )),
                    )
                    .err();
                if let Some(state_error) = retry_error.or(job_error) {
                    return Err(AcmeManagedError::State(state_error));
                }
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
        self.set_job_status(Some(JobStatus::Running));
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
            self.set_job_status(Some(JobStatus::WaitingForChallenge));
            self.write_job(
                job_id,
                JobStatus::WaitingForChallenge,
                unix_now(),
                1,
                None,
                None,
                None,
                None,
            )
            .map_err(AcmeManagedError::State)?;
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
        self.set_job_status(Some(JobStatus::Finalizing));
        self.write_job(
            job_id,
            JobStatus::Finalizing,
            unix_now(),
            1,
            None,
            None,
            None,
            None,
        )
        .map_err(AcmeManagedError::State)?;
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
            client.account().map(|account| account.url.clone()),
            client.directory().document.renewal_info.clone(),
        )
        .map_err(AcmeManagedError::State)?;
        Ok(outcome)
    }

    fn certificate_material(
        &self,
        csr: &oxiroute_acme::LeafCsr,
        certificate_pem: &[u8],
    ) -> Result<(CertificateMaterial, CertificateGeneration), AcmeManagedError> {
        if certificate_pem.len() > MAX_CERTIFICATE_CHAIN_BYTES {
            return Err(AcmeManagedError::CertificateMalformed);
        }
        let certificates = X509::stack_from_pem(certificate_pem)
            .map_err(|_| AcmeManagedError::CertificateMalformed)?;
        if certificates.len() < 2 {
            return Err(AcmeManagedError::CertificateMalformed);
        }
        validate_exact_dns_sans(&certificates[0], &self.declared_dns_names)?;
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
        let serial = certificates[0]
            .serial_number()
            .to_bn()
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
                issuer: Some(issuer_name(&certificates[0])?),
                serial_fingerprint: Some(crate::encoding::lower_hex(&openssl::sha::sha256(
                    &serial.to_vec(),
                ))),
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
        state.last_error_code = error.map(str::to_owned);
    }

    fn set_job_status(&self, status: Option<JobStatus>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job_status = status;
    }

    fn update_schedule(
        &self,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        disk_revision: String,
        outcome: &'static str,
        account_url: Option<String>,
        renewal_info_url: Option<String>,
    ) -> Result<(), AcmeStateError> {
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
        state.retry_attempt = 0;
        state.retry_at_unix_seconds = None;
        state.suggested_renewal_unix_seconds = None;
        state.account_url = account_url.or_else(|| state.account_url.clone());
        state.renewal_info_url = renewal_info_url.or_else(|| state.renewal_info_url.clone());
        state.last_success_unix_seconds = Some(unix_now());
        state.last_outcome = Some(outcome);
        state.last_error_code = None;
        drop(state);
        self.persist_renewal()
    }

    fn schedule_retry(&self, now_unix_seconds: u64) -> Result<(), AcmeStateError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attempt = state.retry_attempt.saturating_add(1);
        let delay = retry_delay(&self.certificate, attempt);
        state.retry_attempt = attempt;
        state.retry_at_unix_seconds = Some(now_unix_seconds.saturating_add(delay));
        state.next_action_unix_seconds = state.retry_at_unix_seconds;
        drop(state);
        self.persist_renewal()
    }

    fn persist_renewal(&self) -> Result<(), AcmeStateError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let renewal = PersistedRenewal {
            certificate: self.certificate.clone(),
            identifiers: self.declared_dns_names.clone(),
            directory_url: self.policy.directory_url.clone(),
            account_url: state.account_url.clone(),
            authenticator: "http01".into(),
            key_type: key_type_string(self.policy.key_type).into(),
            next_action_unix_seconds: state.next_action_unix_seconds,
            retry_at_unix_seconds: state.retry_at_unix_seconds,
            retry_attempt: state.retry_attempt,
            suggested_renewal_unix_seconds: state.suggested_renewal_unix_seconds,
            renewal_info_url: state.renewal_info_url.clone(),
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_error_code: state.last_error_code.clone(),
        };
        drop(state);
        self.revisions.state().write_json(
            &format!("certificates/{}/renewal.json", self.certificate),
            &renewal,
        )
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

fn read_persisted_renewal(
    revisions: &RevisionStore,
    certificate: &str,
) -> Option<PersistedRenewal> {
    revisions
        .state()
        .read_json(
            &format!("certificates/{certificate}/renewal.json"),
            MAX_JOB_BYTES,
        )
        .ok()
}

fn key_type_string(key_type: AcmeKeyType) -> &'static str {
    match key_type {
        AcmeKeyType::EcdsaP256 => "ecdsa_p256",
        AcmeKeyType::Rsa2048 => "rsa_2048",
    }
}

fn is_known_error_code(code: &str) -> bool {
    matches!(
        code,
        "job_busy"
            | "state_failed"
            | "poll_timeout"
            | "transport_failed"
            | "acme_problem"
            | "protocol_failed"
            | "invalid_candidate"
            | "authorization_failed"
            | "order_failed"
            | "certificate_malformed"
            | "account_directory_changed"
            | "publication_conflict"
            | "challenge_failed"
    )
}

fn retry_delay(certificate: &str, attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(8);
    let base = ACME_RETRY_BASE_SECONDS.saturating_mul(1_u64 << exponent);
    let base = base.min(ACME_RETRY_MAX_SECONDS);
    let digest = openssl::sha::sha256(format!("{certificate}:{attempt}").as_bytes());
    let jitter = u64::from(digest[0]) % (base / 4 + 1);
    base.saturating_add(jitter).min(ACME_RETRY_MAX_SECONDS)
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

fn validate_exact_dns_sans(
    certificate: &X509,
    declared_dns_names: &[String],
) -> Result<(), AcmeManagedError> {
    let expected = declared_dns_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let Some(sans) = certificate.subject_alt_names() else {
        return Err(AcmeManagedError::CertificateMalformed);
    };
    let mut actual = BTreeSet::new();
    for san in sans {
        let Some(dns_name) = san.dnsname() else {
            return Err(AcmeManagedError::CertificateMalformed);
        };
        if !actual.insert(dns_name.to_ascii_lowercase()) {
            return Err(AcmeManagedError::CertificateMalformed);
        }
    }
    if actual != expected {
        return Err(AcmeManagedError::CertificateMalformed);
    }
    Ok(())
}

fn issuer_name(certificate: &X509) -> Result<String, AcmeManagedError> {
    let values = certificate
        .issuer_name()
        .entries()
        .map(|entry| {
            entry
                .data()
                .to_string()
                .map_err(|_| AcmeManagedError::CertificateMalformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values.join(","))
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

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{Arc, Mutex},
    };

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use openssl::{
        asn1::Asn1Time,
        bn::{BigNum, MsbOption},
        ec::{EcGroup, EcKey},
        hash::MessageDigest,
        nid::Nid,
        pkey::{PKey, Private},
        x509::{
            extension::{
                AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
                SubjectAlternativeName, SubjectKeyIdentifier,
            },
            X509NameBuilder, X509Req, X509,
        },
    };
    use oxiroute_acme::{
        AcmeTransport, ChallengeStore, HttpRequest, HttpResponse, RevisionStore, StateStore,
        TransportError,
    };
    use oxiroute_config::{AcmeKeyType, SelfSignedKeyType};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;

    struct TestCa {
        certificate: X509,
        key: PKey<Private>,
    }

    #[derive(Clone)]
    struct FakePebbleTransport {
        responses: Arc<Mutex<VecDeque<HttpResponse>>>,
        requests: Arc<Mutex<Vec<String>>>,
        ca: Arc<TestCa>,
        certificate: Arc<Mutex<Option<Vec<u8>>>>,
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
                let payload: Value =
                    serde_json::from_slice(&payload).map_err(|_| TransportError)?;
                let csr = payload
                    .get("csr")
                    .and_then(Value::as_str)
                    .ok_or(TransportError)?;
                let csr = URL_SAFE_NO_PAD.decode(csr).map_err(|_| TransportError)?;
                let csr = X509Req::from_der(&csr).map_err(|_| TransportError)?;
                let certificate = issue_certificate(&csr, &self.ca).map_err(|_| TransportError)?;
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

    #[test]
    fn local_fake_pebble_issues_validates_and_publishes_managed_certificate() {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let names = vec!["proxy.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let reconciler = AcmeManagedReconciler::new(
            "managed",
            names,
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
            },
            revisions.clone(),
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            Arc::clone(&active),
        );
        let transport = FakePebbleTransport {
            responses: Arc::new(Mutex::new(VecDeque::from([
                directory_response(),
                nonce_response("nonce-account"),
                account_response(),
                order_response("pending"),
                authorization_response("pending"),
                challenge_response(),
                authorization_response("valid"),
                order_response("processing"),
                order_response("valid"),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
            ca: Arc::new(test_ca().expect("CA")),
            certificate: Arc::new(Mutex::new(None)),
        };
        let requests = Arc::clone(&transport.requests);
        let outcome = reconciler
            .renew_with_transport(transport)
            .expect("managed issuance");
        assert_eq!(outcome, AcmeManagedOutcome::Activated);
        assert_ne!(reconciler.status().active_revision, "bootstrap");
        let material = revisions
            .load_current("managed")
            .expect("published revision");
        assert!(material.metadata.issuer.is_some());
        assert!(material.metadata.serial_fingerprint.is_some());
        assert!(reconciler.challenge_store.is_empty());
        assert_eq!(reconciler.status().job_status, Some(JobStatus::Succeeded));
        assert!(reconciler.status().last_success_unix_seconds.is_some());
        let renewal =
            fs::read_to_string(temp.path().join("state/certificates/managed/renewal.json"))
                .expect("renewal schedule");
        assert!(renewal.contains("proxy.example.test"));
        assert!(requests
            .lock()
            .expect("request log")
            .iter()
            .all(|url| url.starts_with("https://acme.test/")));
    }

    #[test]
    fn restart_retains_bounded_retry_state_and_redacted_error_code() {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        state
            .write_json(
                "certificates/managed/renewal.json",
                &PersistedRenewal {
                    certificate: "managed".into(),
                    identifiers: vec!["proxy.example.test".into()],
                    directory_url: "https://acme.test/directory".into(),
                    account_url: Some("https://acme.test/acme/acct/1".into()),
                    authenticator: "http01".into(),
                    key_type: "ecdsa_p256".into(),
                    next_action_unix_seconds: Some(2_000),
                    retry_at_unix_seconds: Some(2_000),
                    retry_attempt: 2,
                    suggested_renewal_unix_seconds: None,
                    renewal_info_url: None,
                    last_success_unix_seconds: Some(1_000),
                    last_error_code: Some("transport_failed".into()),
                },
            )
            .expect("renewal state");
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed",
            &["proxy.example.test".into()],
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let reconciler = AcmeManagedReconciler::new(
            "managed",
            vec!["proxy.example.test".into()],
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
            },
            RevisionStore::from_arc(state),
            "revision".into(),
            Some(900),
            Some(4_000),
            false,
            ChallengeStore::default(),
            active,
        );

        let status = reconciler.status();
        assert_eq!(status.retry_attempt, 2);
        assert_eq!(status.next_action_unix_seconds, Some(2_000));
        assert_eq!(status.last_success_unix_seconds, Some(1_000));
        assert_eq!(status.last_error_code.as_deref(), Some("transport_failed"));
    }

    fn directory_response() -> HttpResponse {
        HttpResponse::new(
            200,
            "https://acme.test/directory",
            br#"{"newNonce":"https://acme.test/acme/new-nonce","newAccount":"https://acme.test/acme/new-account","newOrder":"https://acme.test/acme/new-order","meta":{"termsOfService":"https://acme.test/terms"}}"#.to_vec(),
        )
    }

    fn nonce_response(nonce: &str) -> HttpResponse {
        HttpResponse::new(204, "https://acme.test/acme/new-nonce", Vec::new())
            .with_header("replay-nonce", nonce)
    }

    fn account_response() -> HttpResponse {
        HttpResponse::new(
            201,
            "https://acme.test/acme/new-account",
            br#"{"status":"valid","contact":["mailto:ops@example.test"],"termsOfServiceAgreed":true}"#.to_vec(),
        )
        .with_header("location", "https://acme.test/acme/acct/1")
        .with_header("replay-nonce", "nonce-account-response")
    }

    fn order_response(status: &str) -> HttpResponse {
        let body = format!(
            "{{\"status\":\"{status}\",\"identifiers\":[{{\"type\":\"dns\",\"value\":\"proxy.example.test\"}}],\"authorizations\":[\"https://acme.test/acme/authz/1\"],\"finalize\":\"https://acme.test/acme/order/1/finalize\"{}}}",
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

    fn authorization_response(status: &str) -> HttpResponse {
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

    fn challenge_response() -> HttpResponse {
        HttpResponse::new(
            200,
            "https://acme.test/acme/challenge/1",
            br#"{"status":"processing"}"#.to_vec(),
        )
        .with_header("replay-nonce", "nonce-challenge-response")
    }

    fn test_ca() -> Result<TestCa, openssl::error::ErrorStack> {
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
        builder.append_extension(
            SubjectAlternativeName::new()
                .dns("proxy.example.test")
                .build(&context)?,
        )?;
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
}
