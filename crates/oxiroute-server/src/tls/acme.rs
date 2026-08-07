use std::{
    collections::BTreeSet,
    io,
    path::Path,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use openssl::{
    asn1::{Asn1Time, Asn1TimeRef},
    x509::X509,
};
use oxiroute_acme::{
    Account, AccountKey, AccountKeyAlgorithm, AcmeClient, AcmeError, AcmeStateError, AcmeTransport,
    AuthorizationStatus, CertificateMaterial, ChallengeRecord, ChallengeStore, ChallengeStoreError,
    ChallengeType, Dns01Cancellation, Dns01Challenge, Dns01Credentials, Dns01Operation,
    Dns01Provider, Dns01ProviderError, JobState, JobStatus, LeafKeyAlgorithm,
    MAX_DNS01_CREDENTIAL_BYTES, MAX_JOB_BYTES, OriginPolicy, PollPolicy, RedactedOutcome,
    RenewalInformation, RevisionMetadata, RevisionStore, SecretBytes, StateStore,
    SystemAcmeTransport, SystemClock, renewal_due, stable_renewal_time,
    stable_renewal_time_in_window,
};
use oxiroute_config::{AcmeChallengeType, AcmeDns01Config, AcmeKeyType, SelfSignedKeyType};
use serde::{Deserialize, Serialize};

use super::{
    ActiveCertificateGeneration, CertificateGeneration, MAX_CERTIFICATE_CHAIN_BYTES,
    TlsAlpnChallenge, TlsAlpnChallengeStore, TlsBuildError,
};

static NEXT_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const ACME_RETRY_BASE_SECONDS: u64 = 5 * 60;
const ACME_RETRY_MAX_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcmeManagedOutcome {
    Loaded,
    Unchanged,
    Activated,
    Revoked,
    AccountKeyRolledOver,
    Deleted,
}

#[derive(Clone, Debug)]
pub struct AcmeManagedPolicy {
    pub directory_url: String,
    pub contacts: Vec<String>,
    pub terms_agreed: bool,
    pub challenge: AcmeChallengeType,
    pub key_type: AcmeKeyType,
    pub allowed_dns_suffixes: Vec<String>,
    pub retained_revisions: u32,
    pub retention_days: u32,
    pub dns01: Option<AcmeDns01Config>,
}

#[derive(Debug, thiserror::Error)]
pub enum AcmeManagedError {
    #[error("managed ACME job is already running")]
    Busy,
    #[error("managed ACME job is paused")]
    Paused,
    #[error("managed ACME job is not running")]
    NoJob,
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
    #[error("managed ACME account is not configured")]
    AccountNotConfigured,
    #[error("managed ACME publication lost its generation race")]
    Publication,
    #[error("managed ACME challenge could not be provisioned")]
    Challenge(#[source] ChallengeStoreError),
    #[error("managed ACME TLS-ALPN-01 challenge could not be provisioned")]
    TlsAlpnChallenge(#[source] Box<super::TlsAlpnChallengeError>),
    #[error("managed ACME DNS-01 provider is unsupported or not registered")]
    DnsProviderUnsupported,
    #[error("managed ACME DNS-01 credentials are unavailable")]
    DnsCredentials(#[source] Box<TlsBuildError>),
    #[error("managed ACME DNS-01 provider operation failed")]
    DnsProvider(#[source] Dns01ProviderError),
    #[error("managed ACME DNS-01 cleanup failed")]
    DnsCleanup(#[source] Dns01ProviderError),
}

impl AcmeManagedError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Busy => "job_busy",
            Self::Paused => "job_paused",
            Self::NoJob => "job_not_running",
            Self::State(AcmeStateError::PendingDnsCleanup) => "dns_cleanup_pending",
            Self::State(_) => "state_failed",
            Self::Protocol(error) => match error {
                AcmeError::PollTimeout => "poll_timeout",
                AcmeError::Cancelled => "job_cancelled",
                AcmeError::Transport(_) => "transport_failed",
                AcmeError::Problem { .. } => "acme_problem",
                AcmeError::RevocationUnsupported => "revocation_unsupported",
                AcmeError::KeyChangeUnsupported => "key_change_unsupported",
                AcmeError::InvalidCertificate => "invalid_certificate",
                AcmeError::InvalidRevocationReason => "invalid_revocation_reason",
                AcmeError::IpIdentifierUnsupported => "ip_identifier_unsupported",
                AcmeError::InvalidRenewalInformation => "renewal_information_invalid",
                _ => "protocol_failed",
            },
            Self::Tls(_) => "invalid_candidate",
            Self::AuthorizationFailed => "authorization_failed",
            Self::OrderFailed => "order_failed",
            Self::CertificateMalformed => "certificate_malformed",
            Self::AccountDirectoryChanged => "account_directory_changed",
            Self::AccountNotConfigured => "account_not_configured",
            Self::Publication => "publication_conflict",
            Self::Challenge(_) | Self::TlsAlpnChallenge(_) => "challenge_failed",
            Self::DnsProviderUnsupported => "dns_provider_unsupported",
            Self::DnsCredentials(_) => "dns_credentials_failed",
            Self::DnsProvider(error) => match error {
                Dns01ProviderError::Timeout => "dns_provider_timeout",
                Dns01ProviderError::Cancelled => "dns_provider_cancelled",
                _ => "dns_provider_failed",
            },
            Self::DnsCleanup(_) => "dns_cleanup_failed",
        }
    }

    fn is_retryable(&self) -> bool {
        match self {
            Self::Protocol(error) => error.is_retryable(),
            Self::Publication => true,
            Self::Challenge(error) => {
                matches!(
                    error,
                    ChallengeStoreError::DuplicateToken | ChallengeStoreError::CapacityExceeded
                )
            }
            Self::TlsAlpnChallenge(error) => matches!(
                error.as_ref(),
                super::TlsAlpnChallengeError::DuplicateIdentifier
                    | super::TlsAlpnChallengeError::CapacityExceeded
            ),
            Self::DnsProvider(error) => matches!(
                error,
                Dns01ProviderError::ProviderFailed | Dns01ProviderError::Timeout
            ),
            Self::DnsCleanup(_) => true,
            Self::Busy
            | Self::Paused
            | Self::NoJob
            | Self::State(_)
            | Self::Tls(_)
            | Self::AuthorizationFailed
            | Self::OrderFailed
            | Self::CertificateMalformed
            | Self::AccountDirectoryChanged
            | Self::AccountNotConfigured
            | Self::DnsProviderUnsupported
            | Self::DnsCredentials(_) => false,
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
            Self::Revoked => "revoked",
            Self::AccountKeyRolledOver => "account_key_rolled_over",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcmeManagedStatus {
    pub certificate: String,
    pub directory_url: String,
    pub challenge: String,
    pub dns_provider: Option<String>,
    pub key_type: String,
    pub allowed_dns_suffixes: Vec<String>,
    pub disk_revision: String,
    pub active_revision: String,
    pub not_before_unix_seconds: Option<u64>,
    pub not_after_unix_seconds: Option<u64>,
    pub next_action_unix_seconds: Option<u64>,
    pub not_after: String,
    pub job_status: Option<JobStatus>,
    pub job_id: Option<String>,
    pub paused: bool,
    pub retained_revisions: u32,
    pub retention_days: u32,
    pub retry_attempt: u32,
    pub last_success_unix_seconds: Option<u64>,
    pub last_outcome: Option<&'static str>,
    pub last_error_code: Option<String>,
    pub renewal_information_status: &'static str,
    pub dns_provider_deployment: Option<&'static str>,
    pub dns_provider_health: Option<&'static str>,
    pub dns_cleanup_status: &'static str,
}

struct ReconcileState {
    disk_revision: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    retry_attempt: u32,
    retry_at_unix_seconds: Option<u64>,
    suggested_renewal_unix_seconds: Option<u64>,
    auto_retry_blocked: bool,
    account_url: Option<String>,
    renewal_info_url: Option<String>,
    job_status: Option<JobStatus>,
    job_id: Option<String>,
    paused: bool,
    last_success_unix_seconds: Option<u64>,
    last_outcome: Option<&'static str>,
    last_error_code: Option<String>,
    renewal_information_status: &'static str,
    dns_provider_health: &'static str,
    dns_cleanup_status: &'static str,
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
    #[serde(default)]
    dns_provider: Option<String>,
    key_type: String,
    next_action_unix_seconds: Option<u64>,
    retry_at_unix_seconds: Option<u64>,
    retry_attempt: u32,
    suggested_renewal_unix_seconds: Option<u64>,
    #[serde(default)]
    auto_retry_blocked: bool,
    renewal_info_url: Option<String>,
    last_success_unix_seconds: Option<u64>,
    #[serde(default)]
    last_error_code: Option<String>,
    #[serde(default = "default_renewal_information_status")]
    renewal_information_status: String,
    #[serde(default)]
    paused: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedDnsCleanup {
    certificate: String,
    provider: String,
    identifier: String,
    challenge_url: String,
    record_name: String,
    record_value: String,
    provider_record_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcmeDnsCleanupRecovery {
    NotPending,
    Recovered,
    Deferred,
}

#[derive(Default)]
struct JobControl {
    id: Option<String>,
    cancellation: Option<Dns01Cancellation>,
}

/// Owns the managed state root for one certificate and publishes only validated revisions.
pub struct AcmeManagedReconciler {
    certificate: String,
    declared_dns_names: Vec<String>,
    policy: AcmeManagedPolicy,
    revisions: RevisionStore,
    challenge_store: ChallengeStore,
    tls_alpn_challenge_store: TlsAlpnChallengeStore,
    dns_provider: Option<Arc<dyn Dns01Provider>>,
    active: Arc<ActiveCertificateGeneration>,
    job: Mutex<()>,
    control: Mutex<JobControl>,
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
        Self::new_with_dns_provider(
            certificate,
            declared_dns_names,
            policy,
            revisions,
            disk_revision,
            not_before_unix_seconds,
            not_after_unix_seconds,
            initial_issuance_due,
            challenge_store,
            None,
            active,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_with_dns_provider(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        Self::new_with_challenge_stores(
            certificate,
            declared_dns_names,
            policy,
            revisions,
            disk_revision,
            not_before_unix_seconds,
            not_after_unix_seconds,
            initial_issuance_due,
            challenge_store,
            TlsAlpnChallengeStore::default(),
            dns_provider,
            active,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn new_with_challenge_stores(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        policy: AcmeManagedPolicy,
        revisions: RevisionStore,
        disk_revision: String,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        initial_issuance_due: bool,
        challenge_store: ChallengeStore,
        tls_alpn_challenge_store: TlsAlpnChallengeStore,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
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
                    && renewal.authenticator == challenge_string(policy.challenge)
                    && renewal.dns_provider
                        == policy.dns01.as_ref().map(|dns01| dns01.provider.clone())
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
        let auto_retry_blocked = persisted
            .as_ref()
            .is_some_and(|renewal| renewal.auto_retry_blocked);
        let next_action_unix_seconds = if auto_retry_blocked {
            None
        } else {
            next_action_unix_seconds
        };
        let paused = persisted.as_ref().is_some_and(|renewal| renewal.paused);
        let next_action_unix_seconds = if paused {
            None
        } else {
            next_action_unix_seconds
        };
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
        let renewal_information_status = persisted
            .as_ref()
            .and_then(|renewal| {
                normalized_renewal_information_status(&renewal.renewal_information_status)
            })
            .unwrap_or(if renewal_info_url.is_some() {
                "pending"
            } else {
                "not_advertised"
            });
        let last_success_unix_seconds = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_success_unix_seconds);
        let last_error_code = persisted
            .as_ref()
            .and_then(|renewal| renewal.last_error_code.as_deref())
            .filter(|code| is_known_error_code(code))
            .map(str::to_owned);
        let last_outcome = Some(AcmeManagedOutcome::Loaded.code());
        let dns_cleanup_status = if policy.challenge == AcmeChallengeType::Dns01 {
            persisted_dns_cleanup_status(&revisions, &certificate)
        } else {
            "not_applicable"
        };
        let dns_provider_health = if policy.challenge != AcmeChallengeType::Dns01 {
            "not_applicable"
        } else if dns_provider.is_none() {
            "unsupported"
        } else if matches!(dns_cleanup_status, "pending" | "failed") {
            "degraded"
        } else {
            "unknown"
        };
        let reconciler = Self {
            certificate,
            declared_dns_names,
            policy,
            revisions,
            challenge_store,
            tls_alpn_challenge_store,
            dns_provider,
            active,
            job: Mutex::new(()),
            control: Mutex::new(JobControl::default()),
            state: Mutex::new(ReconcileState {
                disk_revision,
                not_before_unix_seconds,
                not_after_unix_seconds,
                next_action_unix_seconds,
                retry_attempt,
                retry_at_unix_seconds,
                suggested_renewal_unix_seconds,
                auto_retry_blocked,
                account_url,
                renewal_info_url,
                job_status: paused.then_some(JobStatus::Paused),
                job_id: None,
                paused,
                last_success_unix_seconds,
                last_outcome,
                last_error_code,
                renewal_information_status,
                dns_provider_health,
                dns_cleanup_status,
            }),
        };
        if reconciler.dns_provider.is_some() {
            let _ = reconciler.recover_pending_dns_cleanup();
        }
        reconciler
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
            challenge: challenge_string(self.policy.challenge).into(),
            dns_provider: self
                .policy
                .dns01
                .as_ref()
                .map(|dns01| dns01.provider.clone()),
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
            job_id: state.job_id.clone(),
            paused: state.paused,
            retained_revisions: self.policy.retained_revisions,
            retention_days: self.policy.retention_days,
            retry_attempt: state.retry_attempt,
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code.clone(),
            renewal_information_status: state.renewal_information_status,
            dns_provider_deployment: (self.policy.challenge == AcmeChallengeType::Dns01).then_some(
                if self.dns_provider.is_some() {
                    "registered"
                } else {
                    "unsupported"
                },
            ),
            dns_provider_health: (self.policy.challenge == AcmeChallengeType::Dns01)
                .then_some(state.dns_provider_health),
            dns_cleanup_status: state.dns_cleanup_status,
        }
    }

    #[must_use]
    pub fn renewal_due(&self, now_unix_seconds: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.paused || state.auto_retry_blocked {
            return false;
        }
        if let Some(next) = state.next_action_unix_seconds {
            return now_unix_seconds >= next;
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
        self.renew_now_with_correlation("system")
    }

    /// Runs one bounded managed issuance or renewal and records the caller correlation ID.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when account, challenge, ACME, storage, or publication
    /// steps fail.
    pub fn renew_now_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.renew_with_provider(
            SystemAcmeTransport::default(),
            self.dns_provider.clone(),
            correlation_id.into(),
        )
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
        self.renew_with_provider(transport, self.dns_provider.clone(), "test".into())
    }

    /// Requests cooperative cancellation of the currently running job.
    ///
    /// # Errors
    ///
    /// Returns [`AcmeManagedError::NoJob`] when no managed job is running.
    pub fn cancel_job(&self) -> Result<String, AcmeManagedError> {
        let control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(id) = control.id.clone() else {
            return Err(AcmeManagedError::NoJob);
        };
        if let Some(cancellation) = &control.cancellation {
            cancellation.cancel();
        }
        Ok(id)
    }

    /// Pauses automatic issuance and cooperatively stops the current job, if any.
    ///
    /// # Errors
    ///
    /// Returns a state error when the durable pause marker cannot be written.
    pub fn pause(&self) -> Result<Option<String>, AcmeManagedError> {
        let id = {
            let control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cancellation) = &control.cancellation {
                cancellation.cancel();
            }
            control.id.clone()
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.paused = true;
            state.job_status = Some(JobStatus::Paused);
            state.next_action_unix_seconds = None;
        }
        self.persist_renewal().map_err(AcmeManagedError::State)?;
        Ok(id)
    }

    /// Resumes automatic issuance using the current certificate schedule.
    ///
    /// # Errors
    ///
    /// Returns a state error when the durable pause marker cannot be written.
    pub fn resume(&self) -> Result<(), AcmeManagedError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.paused = false;
            state.job_status = None;
            state.next_action_unix_seconds = restored_next_action(&state, &self.certificate);
        }
        self.persist_renewal().map_err(AcmeManagedError::State)
    }

    /// Revokes the currently selected leaf certificate without removing its active material.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the account, certificate, ACME endpoint, or state
    /// operation fails.
    pub fn revoke_now_with_correlation(
        &self,
        reason: Option<u8>,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let (job_id, now) = self.begin_action_job("revoke", &correlation_id)?;
        let result = (|| {
            let material = self
                .revisions
                .load_current(&self.certificate)
                .map_err(AcmeManagedError::State)?;
            let mut client = self.client_with_account(SystemAcmeTransport::default(), false)?;
            client
                .revoke_certificate(&material.certificate_pem, reason)
                .map_err(AcmeManagedError::Protocol)
        })();
        let outcome = AcmeManagedOutcome::Revoked;
        let finish_result =
            self.finish_action_job(&job_id, "revoke", now, &correlation_id, &result, outcome);
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }

    /// Rolls the account key and persists it only after the ACME server accepts the nested JWS.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the account, key, ACME endpoint, or state
    /// operation fails.
    pub fn rollover_account_key_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let (job_id, now) = self.begin_action_job("account_rollover", &correlation_id)?;
        let result = (|| {
            let new_key = AccountKey::generate(account_key_algorithm(self.policy.key_type))
                .map_err(AcmeManagedError::Protocol)?;
            let mut client = self.client_with_account(SystemAcmeTransport::default(), false)?;
            let account = client
                .rollover_account_key(new_key.clone())
                .map_err(AcmeManagedError::Protocol)?;
            self.revisions
                .state()
                .write_secret(
                    &format!("accounts/{}/account-key.pem", self.certificate),
                    new_key.private_key_pem(),
                )
                .map_err(AcmeManagedError::State)?;
            self.revisions
                .state()
                .write_json(
                    &format!("accounts/{}/account.json", self.certificate),
                    &PersistedAccount {
                        directory_url: self.policy.directory_url.clone(),
                        account,
                    },
                )
                .map_err(AcmeManagedError::State)
        })();
        let outcome = AcmeManagedOutcome::AccountKeyRolledOver;
        let finish_result = self.finish_action_job(
            &job_id,
            "account_rollover",
            now,
            &correlation_id,
            &result,
            outcome,
        );
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }

    /// Deletes persisted managed ACME state after the caller has removed active references.
    ///
    /// # Errors
    ///
    /// Returns a redacted managed-job error when the state cannot be deleted.
    pub fn delete_state_with_correlation(
        &self,
        correlation_id: impl Into<String>,
    ) -> Result<(AcmeManagedOutcome, String), AcmeManagedError> {
        let _job = self.try_lock_job()?;
        let correlation_id = correlation_id.into();
        let (job_id, now) = self.begin_action_job("delete", &correlation_id)?;
        let result = self
            .revisions
            .delete_certificate_state(&self.certificate)
            .map_err(AcmeManagedError::State);
        let outcome = AcmeManagedOutcome::Deleted;
        let finish_result =
            self.finish_action_job(&job_id, "delete", now, &correlation_id, &result, outcome);
        self.set_job_id(None);
        finish_result?;
        result.map(|()| (outcome, job_id))
    }

    #[allow(clippy::too_many_lines)]
    fn renew_with_provider<T: AcmeTransport>(
        &self,
        transport: T,
        provider: Option<Arc<dyn Dns01Provider>>,
        correlation_id: String,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        let _job = match self.job.try_lock() {
            Ok(job) => job,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(AcmeManagedError::Busy),
        };
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .paused
        {
            return Err(AcmeManagedError::Paused);
        }
        let now = unix_now();
        let sequence = NEXT_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("renew-{now}-{sequence}");
        let cancellation = Dns01Cancellation::new();
        {
            let mut control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            control.id = Some(job_id.clone());
            control.cancellation = Some(cancellation.clone());
        }
        self.set_job_id(Some(job_id.clone()));
        self.set_job_status(Some(JobStatus::Queued));
        if let Err(error) = self.write_job(
            &job_id,
            "renew",
            JobStatus::Queued,
            now,
            0,
            None,
            None,
            None,
            None,
            Some(correlation_id.clone()),
        ) {
            self.clear_job_control(&job_id);
            return Err(AcmeManagedError::State(error));
        }
        let result = self.renew_inner(
            transport,
            &job_id,
            now,
            provider,
            cancellation,
            &correlation_id,
        );
        match &result {
            Ok(outcome) => {
                let status = self.status();
                self.set_outcome(Some(outcome.code()), None);
                self.set_job_status(Some(JobStatus::Succeeded));
                let write_result = self.write_job(
                    &job_id,
                    "renew",
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
                    Some(correlation_id.clone()),
                );
                if let Err(error) = write_result {
                    self.clear_job_control(&job_id);
                    return Err(AcmeManagedError::State(error));
                }
            }
            Err(error) => {
                let paused = self.is_paused();
                let cancelled = is_cancelled_error(error);
                if !paused && !cancelled {
                    if error.is_retryable() {
                        self.schedule_retry(unix_now());
                    } else {
                        self.block_automatic_retry();
                    }
                }
                self.set_outcome(
                    if paused {
                        Some("paused")
                    } else if cancelled {
                        Some("cancelled")
                    } else {
                        None
                    },
                    if paused || cancelled {
                        None
                    } else {
                        Some(error.code())
                    },
                );
                let renewal_error = if paused || cancelled {
                    None
                } else {
                    self.persist_renewal().err()
                };
                if renewal_error.is_some() {
                    self.set_outcome(None, Some("state_failed"));
                }
                let job_status = if paused {
                    JobStatus::Paused
                } else if cancelled {
                    JobStatus::Cancelled
                } else {
                    JobStatus::Failed
                };
                self.set_job_status(Some(job_status.clone()));
                let status = self.status();
                let outcome_code = if renewal_error.is_some() {
                    "state_failed"
                } else if paused {
                    "paused"
                } else if cancelled {
                    "cancelled"
                } else {
                    error.code()
                };
                let job_error = self
                    .write_job(
                        &job_id,
                        "renew",
                        job_status,
                        unix_now(),
                        1,
                        None,
                        Some(status.disk_revision),
                        Some(status.active_revision),
                        Some(RedactedOutcome::new(
                            outcome_code,
                            "managed ACME renewal failed",
                        )),
                        Some(correlation_id),
                    )
                    .err();
                if let Some(state_error) = renewal_error.or(job_error) {
                    self.clear_job_control(&job_id);
                    return Err(AcmeManagedError::State(state_error));
                }
            }
        }
        self.clear_job_control(&job_id);
        result
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn renew_inner<T: AcmeTransport>(
        &self,
        transport: T,
        job_id: &str,
        created_at: u64,
        dns_provider: Option<Arc<dyn Dns01Provider>>,
        cancellation: Dns01Cancellation,
        correlation_id: &str,
    ) -> Result<AcmeManagedOutcome, AcmeManagedError> {
        self.write_job(
            job_id,
            "renew",
            JobStatus::Running,
            unix_now(),
            1,
            None,
            None,
            None,
            None,
            Some(correlation_id.into()),
        )
        .map_err(AcmeManagedError::State)?;
        self.set_job_status(Some(JobStatus::Running));
        if self.policy.challenge == AcmeChallengeType::Dns01
            && self.recover_pending_dns_cleanup() == AcmeDnsCleanupRecovery::Deferred
        {
            return Err(AcmeManagedError::DnsCleanup(
                Dns01ProviderError::CleanupFailed,
            ));
        }
        let mut client = self.client_with_account(transport, true)?;
        let dns_context = match self.policy.challenge {
            AcmeChallengeType::Dns01 => {
                let dns01 = self
                    .policy
                    .dns01
                    .as_ref()
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let provider = dns_provider
                    .as_ref()
                    .filter(|provider| provider.name() == dns01.provider)
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let credentials = load_dns_credentials(&dns01.credential_file, &self.certificate)?;
                Some((Arc::clone(provider), credentials, dns01.timeout_seconds))
            }
            AcmeChallengeType::Http01 | AcmeChallengeType::TlsAlpn01 => None,
        };
        let order = client
            .create_order(&oxiroute_acme::CertificateRequest {
                identifiers: self.declared_dns_names.clone(),
            })
            .map_err(AcmeManagedError::Protocol)?;
        let declared_identifiers = self
            .declared_dns_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut authorized_identifiers = BTreeSet::new();
        for (index, authorization_url) in order.authorizations.iter().enumerate() {
            self.set_job_status(Some(JobStatus::WaitingForChallenge));
            self.write_job(
                job_id,
                "renew",
                JobStatus::WaitingForChallenge,
                unix_now(),
                1,
                None,
                None,
                None,
                None,
                Some(correlation_id.into()),
            )
            .map_err(AcmeManagedError::State)?;
            let authorization = match self.policy.challenge {
                AcmeChallengeType::Http01 => client.authorization(authorization_url),
                AcmeChallengeType::Dns01 => {
                    client.authorization_for(authorization_url, ChallengeType::Dns01)
                }
                AcmeChallengeType::TlsAlpn01 => {
                    client.authorization_for(authorization_url, ChallengeType::TlsAlpn01)
                }
            }
            .map_err(AcmeManagedError::Protocol)?;
            if !declared_identifiers.contains(&authorization.identifier)
                || !authorized_identifiers.insert(authorization.identifier.clone())
            {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
            if authorization.status == AuthorizationStatus::Valid {
                continue;
            }
            if !matches!(
                authorization.status,
                AuthorizationStatus::Pending | AuthorizationStatus::Processing
            ) {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
            if self.policy.challenge == AcmeChallengeType::Dns01 {
                let (provider, credentials, timeout_seconds) = dns_context
                    .as_ref()
                    .ok_or(AcmeManagedError::DnsProviderUnsupported)?;
                let challenge = authorization
                    .dns01_challenge
                    .as_ref()
                    .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
                self.complete_dns01_challenge(
                    &mut client,
                    &authorization.url,
                    challenge,
                    provider.as_ref(),
                    credentials,
                    *timeout_seconds,
                    cancellation.clone(),
                )?;
                continue;
            }
            if self.policy.challenge == AcmeChallengeType::TlsAlpn01 {
                let challenge = authorization
                    .tls_alpn01_challenge
                    .as_ref()
                    .ok_or(AcmeManagedError::Protocol(AcmeError::UnsupportedChallenge))?;
                let created_at = unix_now();
                let identity = TlsAlpnChallenge::generate(
                    &authorization.identifier,
                    &challenge.key_authorization,
                    "managed-account",
                    job_id,
                    &format!("authorization-{index}"),
                    &format!("challenge-{index}"),
                    created_at,
                    created_at.saturating_add(600),
                )
                .map_err(|error| AcmeManagedError::TlsAlpnChallenge(Box::new(error)))?;
                let lease = self
                    .tls_alpn_challenge_store
                    .provision(identity)
                    .map_err(|error| AcmeManagedError::TlsAlpnChallenge(Box::new(error)))?;
                client
                    .respond_to_tls_alpn01_challenge(challenge)
                    .map_err(AcmeManagedError::Protocol)?;
                let authorization = client
                    .poll_authorization_for(
                        &authorization.url,
                        &poll_policy(unix_now().saturating_add(600), Some(cancellation.clone())),
                        ChallengeType::TlsAlpn01,
                    )
                    .map_err(AcmeManagedError::Protocol)?;
                lease.complete();
                if authorization.status != AuthorizationStatus::Valid {
                    return Err(AcmeManagedError::AuthorizationFailed);
                }
                continue;
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
                    &poll_policy(unix_now().saturating_add(600), Some(cancellation.clone())),
                )
                .map_err(AcmeManagedError::Protocol)?;
            lease.complete();
            if authorization.status != AuthorizationStatus::Valid {
                return Err(AcmeManagedError::AuthorizationFailed);
            }
        }
        if authorized_identifiers != declared_identifiers {
            return Err(AcmeManagedError::AuthorizationFailed);
        }
        self.set_job_status(Some(JobStatus::Finalizing));
        self.write_job(
            job_id,
            "renew",
            JobStatus::Finalizing,
            unix_now(),
            1,
            None,
            None,
            None,
            None,
            Some(correlation_id.into()),
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
            .poll_order(
                &finalized.url,
                &poll_policy(unix_now().saturating_add(600), Some(cancellation)),
            )
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
        let (renewal_information, renewal_information_status) =
            match client.renewal_information(&material.certificate_pem) {
                Ok(Some(information)) => {
                    let usable = material
                        .metadata
                        .not_before_unix_seconds
                        .zip(material.metadata.not_after_unix_seconds)
                        .is_some_and(|(not_before, not_after)| {
                            stable_renewal_time_in_window(
                                information.suggested_window_start_unix_seconds,
                                information.suggested_window_end_unix_seconds,
                                not_before,
                                not_after,
                                &self.certificate,
                            )
                            .is_some()
                        });
                    (
                        Some(information),
                        if usable { "applied" } else { "invalid" },
                    )
                }
                Ok(None) => (None, "not_advertised"),
                Err(AcmeError::InvalidRenewalInformation) => (None, "invalid"),
                Err(_) => (None, "unavailable"),
            };
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
            renewal_information.as_ref(),
            renewal_information_status,
        )
        .map_err(AcmeManagedError::State)?;
        Ok(outcome)
    }

    fn client_with_account<T: AcmeTransport>(
        &self,
        transport: T,
        register_if_missing: bool,
    ) -> Result<AcmeClient<T>, AcmeManagedError> {
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
            Err(AcmeStateError::FileOpen(error))
                if error.kind() == io::ErrorKind::NotFound && register_if_missing =>
            {
                let key =
                    AccountKey::generate(key_algorithm).map_err(AcmeManagedError::Protocol)?;
                self.revisions
                    .state()
                    .write_secret(&account_key_path, key.private_key_pem())
                    .map_err(AcmeManagedError::State)?;
                key
            }
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AcmeManagedError::AccountNotConfigured);
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
            Err(AcmeStateError::FileOpen(error))
                if error.kind() == io::ErrorKind::NotFound && register_if_missing =>
            {
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
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AcmeManagedError::AccountNotConfigured);
            }
            Err(error) => return Err(AcmeManagedError::State(error)),
        }
        Ok(client)
    }

    /// Attempts one bounded recovery of a durable DNS-01 cleanup journal.
    #[must_use]
    pub fn recover_pending_dns_cleanup(&self) -> AcmeDnsCleanupRecovery {
        if self.policy.challenge != AcmeChallengeType::Dns01 {
            return AcmeDnsCleanupRecovery::NotPending;
        }
        let journal = match self
            .revisions
            .state()
            .read_secret_json::<PersistedDnsCleanup>(
                &dns_cleanup_path(&self.certificate),
                MAX_JOB_BYTES,
            ) {
            Ok(journal) => journal,
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {
                let cleanup_failed = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .dns_cleanup_status
                    == "failed";
                return if cleanup_failed {
                    AcmeDnsCleanupRecovery::Deferred
                } else {
                    AcmeDnsCleanupRecovery::NotPending
                };
            }
            Err(_) => {
                self.set_dns_status("failed", "degraded");
                return AcmeDnsCleanupRecovery::Deferred;
            }
        };
        let Some(provider) = self.dns_provider.as_ref() else {
            self.set_dns_status("failed", "unsupported");
            return AcmeDnsCleanupRecovery::Deferred;
        };
        let Some(dns01) = self.policy.dns01.as_ref() else {
            self.set_dns_status("failed", "unsupported");
            return AcmeDnsCleanupRecovery::Deferred;
        };
        if journal.provider != dns01.provider || provider.name() != journal.provider {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        }
        let Ok(record) = persisted_dns_record(&self.certificate, &journal) else {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        };
        let Ok(credentials) = load_dns_credentials(&dns01.credential_file, &self.certificate)
        else {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        };
        let Ok(operation) = Dns01Operation::new(Duration::from_secs(dns01.timeout_seconds)) else {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        };
        if provider
            .cleanup_txt_record(&record, &credentials, &operation)
            .is_err()
        {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        }
        if self
            .revisions
            .state()
            .remove_file(&dns_cleanup_path(&self.certificate))
            .is_err()
        {
            self.set_dns_status("failed", "degraded");
            return AcmeDnsCleanupRecovery::Deferred;
        }
        self.set_dns_status("recovered", "healthy");
        AcmeDnsCleanupRecovery::Recovered
    }

    #[allow(
        clippy::needless_pass_by_value,
        clippy::too_many_arguments,
        clippy::unused_self
    )]
    fn complete_dns01_challenge<T: AcmeTransport>(
        &self,
        client: &mut AcmeClient<T>,
        authorization_url: &str,
        challenge: &Dns01Challenge,
        provider: &dyn Dns01Provider,
        credentials: &Dns01Credentials,
        timeout_seconds: u64,
        cancellation: Dns01Cancellation,
    ) -> Result<(), AcmeManagedError> {
        let timeout = Duration::from_secs(timeout_seconds);
        let operation =
            Dns01Operation::with_cancellation(timeout, cancellation.clone()).map_err(|error| {
                self.set_dns_status("none", "degraded");
                AcmeManagedError::DnsProvider(error)
            })?;
        operation.check().map_err(|error| {
            self.set_dns_status("none", "degraded");
            AcmeManagedError::DnsProvider(error)
        })?;
        let record = provider
            .create_txt_record(challenge, credentials, &operation)
            .map_err(|error| {
                self.set_dns_status("none", "degraded");
                AcmeManagedError::DnsProvider(error)
            })?;
        if !record.matches(challenge, provider.name()) {
            self.set_dns_status("none", "degraded");
            return Err(AcmeManagedError::DnsProvider(
                Dns01ProviderError::InvalidRecord,
            ));
        }
        self.persist_dns_cleanup(challenge, &record)
            .inspect_err(|_| {
                self.set_dns_status("failed", "degraded");
                let cleanup_operation = Dns01Operation::new(timeout);
                if let Ok(cleanup_operation) = cleanup_operation {
                    if provider
                        .cleanup_txt_record(&record, credentials, &cleanup_operation)
                        .is_ok()
                    {
                        self.set_dns_status("recovered", "healthy");
                    }
                }
            })?;
        let authorization_result =
            match provider.wait_for_propagation(challenge, &record, credentials, &operation) {
                Ok(()) => (|| {
                    operation.check().map_err(|error| {
                        self.set_dns_status("failed", "degraded");
                        AcmeManagedError::DnsProvider(error)
                    })?;
                    client
                        .respond_to_dns01_challenge(challenge)
                        .map_err(AcmeManagedError::Protocol)?;
                    let authorization = client
                        .poll_authorization_for(
                            authorization_url,
                            &poll_policy(
                                unix_now().saturating_add(600),
                                Some(cancellation.clone()),
                            ),
                            ChallengeType::Dns01,
                        )
                        .map_err(AcmeManagedError::Protocol)?;
                    if authorization.status != AuthorizationStatus::Valid {
                        return Err(AcmeManagedError::AuthorizationFailed);
                    }
                    Ok(())
                })(),
                Err(error) => {
                    self.set_dns_status("failed", "degraded");
                    Err(AcmeManagedError::DnsProvider(error))
                }
            };

        self.cleanup_dns_record(provider, credentials, &record, timeout)?;
        authorization_result
    }

    fn persist_dns_cleanup(
        &self,
        challenge: &Dns01Challenge,
        record: &oxiroute_acme::Dns01Record,
    ) -> Result<(), AcmeManagedError> {
        self.revisions
            .state()
            .write_secret_json(
                &dns_cleanup_path(&self.certificate),
                &PersistedDnsCleanup {
                    certificate: self.certificate.clone(),
                    provider: record.provider().into(),
                    identifier: challenge.identifier().into(),
                    challenge_url: challenge.challenge_url().into(),
                    record_name: challenge.record_name().into(),
                    record_value: challenge.record_value().into(),
                    provider_record_id: record.provider_record_id().into(),
                },
            )
            .map_err(AcmeManagedError::State)
    }

    fn cleanup_dns_record(
        &self,
        provider: &dyn Dns01Provider,
        credentials: &Dns01Credentials,
        record: &oxiroute_acme::Dns01Record,
        timeout: Duration,
    ) -> Result<(), AcmeManagedError> {
        let cleanup_operation = Dns01Operation::new(timeout).map_err(|error| {
            self.set_dns_status("failed", "degraded");
            AcmeManagedError::DnsCleanup(error)
        })?;
        cleanup_operation.check().map_err(|error| {
            self.set_dns_status("failed", "degraded");
            AcmeManagedError::DnsCleanup(error)
        })?;
        provider
            .cleanup_txt_record(record, credentials, &cleanup_operation)
            .map_err(|error| {
                self.set_dns_status("failed", "degraded");
                AcmeManagedError::DnsCleanup(error)
            })?;
        self.revisions
            .state()
            .remove_file(&dns_cleanup_path(&self.certificate))
            .map_err(|error| {
                self.set_dns_status("failed", "degraded");
                AcmeManagedError::State(error)
            })?;
        self.set_dns_status("recovered", "healthy");
        Ok(())
    }

    fn set_dns_status(&self, cleanup_status: &'static str, provider_health: &'static str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dns_cleanup_status = cleanup_status;
        state.dns_provider_health = provider_health;
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
        operation: &str,
        status: JobStatus,
        now: u64,
        attempt: u32,
        next_action_unix_seconds: Option<u64>,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        last_outcome: Option<RedactedOutcome>,
        correlation_id: Option<String>,
    ) -> Result<(), AcmeStateError> {
        self.revisions.state().write_job(&JobState {
            id: id.into(),
            certificate: self.certificate.clone(),
            operation: operation.into(),
            status,
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
            attempt,
            next_action_unix_seconds,
            disk_revision,
            active_revision,
            last_outcome,
            correlation_id,
        })
    }

    fn try_lock_job(&self) -> Result<std::sync::MutexGuard<'_, ()>, AcmeManagedError> {
        match self.job.try_lock() {
            Ok(job) => Ok(job),
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => Err(AcmeManagedError::Busy),
        }
    }

    fn begin_action_job(
        &self,
        operation: &str,
        correlation_id: &str,
    ) -> Result<(String, u64), AcmeManagedError> {
        let now = unix_now();
        let sequence = NEXT_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("{operation}-{now}-{sequence}");
        self.set_job_id(Some(job_id.clone()));
        self.set_job_status(Some(JobStatus::Queued));
        if let Err(error) = self.write_job(
            &job_id,
            operation,
            JobStatus::Queued,
            now,
            0,
            None,
            None,
            None,
            None,
            Some(correlation_id.into()),
        ) {
            self.set_job_id(None);
            self.set_job_status(None);
            return Err(AcmeManagedError::State(error));
        }
        Ok((job_id, now))
    }

    fn finish_action_job(
        &self,
        job_id: &str,
        operation: &str,
        created_at: u64,
        correlation_id: &str,
        result: &Result<(), AcmeManagedError>,
        success: AcmeManagedOutcome,
    ) -> Result<(), AcmeManagedError> {
        let status = self.status();
        let (job_status, outcome_code, outcome, error_code) = match result {
            Ok(()) => (
                JobStatus::Succeeded,
                success.code(),
                Some(RedactedOutcome::new(
                    success.code(),
                    "managed ACME action completed",
                )),
                None,
            ),
            Err(error) => (
                JobStatus::Failed,
                error.code(),
                Some(RedactedOutcome::new(
                    error.code(),
                    "managed ACME action failed",
                )),
                Some(error.code()),
            ),
        };
        self.set_outcome(Some(outcome_code), error_code);
        self.set_job_status(Some(job_status.clone()));
        self.write_job(
            job_id,
            operation,
            job_status,
            created_at,
            1,
            None,
            Some(status.disk_revision),
            Some(status.active_revision),
            outcome,
            Some(correlation_id.into()),
        )
        .map_err(AcmeManagedError::State)
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

    fn set_job_id(&self, id: Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .job_id = id;
    }

    fn is_paused(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .paused
    }

    fn clear_job_control(&self, id: &str) {
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if control.id.as_deref() == Some(id) {
            control.id = None;
            control.cancellation = None;
            self.set_job_id(None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_schedule(
        &self,
        not_before_unix_seconds: Option<u64>,
        not_after_unix_seconds: Option<u64>,
        disk_revision: String,
        outcome: &'static str,
        account_url: Option<String>,
        renewal_info_url: Option<String>,
        renewal_information: Option<&RenewalInformation>,
        renewal_information_status: &'static str,
    ) -> Result<(), AcmeStateError> {
        let local_next_action_unix_seconds = not_before_unix_seconds.and_then(|not_before| {
            not_after_unix_seconds
                .and_then(|not_after| stable_renewal_time(not_before, not_after, &self.certificate))
        });
        let suggested_renewal_unix_seconds = renewal_information
            .zip(not_before_unix_seconds.zip(not_after_unix_seconds))
            .and_then(|(information, (not_before, not_after))| {
                stable_renewal_time_in_window(
                    information.suggested_window_start_unix_seconds,
                    information.suggested_window_end_unix_seconds,
                    not_before,
                    not_after,
                    &self.certificate,
                )
            });
        let next_action_unix_seconds =
            suggested_renewal_unix_seconds.or(local_next_action_unix_seconds);
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
        state.suggested_renewal_unix_seconds = suggested_renewal_unix_seconds;
        state.auto_retry_blocked = false;
        state.account_url = account_url.or_else(|| state.account_url.clone());
        state.renewal_info_url = renewal_info_url.or_else(|| state.renewal_info_url.clone());
        state.renewal_information_status = renewal_information_status;
        state.last_success_unix_seconds = Some(unix_now());
        state.last_outcome = Some(outcome);
        state.last_error_code = None;
        drop(state);
        self.persist_renewal()?;
        self.revisions.garbage_collect(
            &self.certificate,
            self.policy.retained_revisions as usize,
            unix_now().saturating_sub(u64::from(self.policy.retention_days) * 86_400),
        )?;
        Ok(())
    }

    fn schedule_retry(&self, now_unix_seconds: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attempt = state.retry_attempt.saturating_add(1);
        let delay = retry_delay(&self.certificate, attempt);
        state.retry_attempt = attempt;
        state.retry_at_unix_seconds = Some(now_unix_seconds.saturating_add(delay));
        state.next_action_unix_seconds = state.retry_at_unix_seconds;
        state.auto_retry_blocked = false;
    }

    fn block_automatic_retry(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retry_at_unix_seconds = None;
        state.next_action_unix_seconds = None;
        state.auto_retry_blocked = true;
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
            authenticator: challenge_string(self.policy.challenge).into(),
            dns_provider: self
                .policy
                .dns01
                .as_ref()
                .map(|dns01| dns01.provider.clone()),
            key_type: key_type_string(self.policy.key_type).into(),
            next_action_unix_seconds: state.next_action_unix_seconds,
            retry_at_unix_seconds: state.retry_at_unix_seconds,
            retry_attempt: state.retry_attempt,
            suggested_renewal_unix_seconds: state.suggested_renewal_unix_seconds,
            auto_retry_blocked: state.auto_retry_blocked,
            renewal_info_url: state.renewal_info_url.clone(),
            last_success_unix_seconds: state.last_success_unix_seconds,
            last_error_code: state.last_error_code.clone(),
            renewal_information_status: state.renewal_information_status.into(),
            paused: state.paused,
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

fn load_dns_credentials(
    path: &Path,
    certificate: &str,
) -> Result<Dns01Credentials, AcmeManagedError> {
    let bytes = super::read_bounded_stable(
        certificate,
        "DNS-01 credentials",
        path,
        MAX_DNS01_CREDENTIAL_BYTES,
        true,
    )
    .map_err(|source| AcmeManagedError::DnsCredentials(Box::new(source)))?;
    Dns01Credentials::new(bytes.to_vec()).map_err(AcmeManagedError::DnsProvider)
}

fn dns_cleanup_path(certificate: &str) -> String {
    format!("certificates/{certificate}/dns-cleanup.json")
}

fn persisted_dns_cleanup_status(revisions: &RevisionStore, certificate: &str) -> &'static str {
    match revisions
        .state()
        .read_secret_json::<PersistedDnsCleanup>(&dns_cleanup_path(certificate), MAX_JOB_BYTES)
    {
        Ok(_) => "pending",
        Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => "none",
        Err(_) => "failed",
    }
}

fn persisted_dns_record(
    certificate: &str,
    journal: &PersistedDnsCleanup,
) -> Result<oxiroute_acme::Dns01Record, Dns01ProviderError> {
    if journal.certificate != certificate {
        return Err(Dns01ProviderError::InvalidRecord);
    }
    let challenge = Dns01Challenge::new(
        &journal.identifier,
        &journal.challenge_url,
        &journal.record_name,
        &journal.record_value,
    )?;
    let record = oxiroute_acme::Dns01Record::new(
        &journal.provider,
        &journal.challenge_url,
        &journal.record_name,
        journal.record_value.as_bytes().to_vec(),
        &journal.provider_record_id,
    )?;
    if !record.matches(&challenge, &journal.provider) {
        return Err(Dns01ProviderError::InvalidRecord);
    }
    Ok(record)
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

fn challenge_string(challenge: AcmeChallengeType) -> &'static str {
    match challenge {
        AcmeChallengeType::Http01 => "http01",
        AcmeChallengeType::Dns01 => "dns01",
        AcmeChallengeType::TlsAlpn01 => "tls_alpn01",
    }
}

fn is_known_error_code(code: &str) -> bool {
    matches!(
        code,
        "job_busy"
            | "state_failed"
            | "poll_timeout"
            | "job_cancelled"
            | "transport_failed"
            | "acme_problem"
            | "protocol_failed"
            | "invalid_candidate"
            | "authorization_failed"
            | "order_failed"
            | "certificate_malformed"
            | "account_directory_changed"
            | "account_not_configured"
            | "revocation_unsupported"
            | "key_change_unsupported"
            | "invalid_certificate"
            | "invalid_revocation_reason"
            | "ip_identifier_unsupported"
            | "renewal_information_invalid"
            | "publication_conflict"
            | "challenge_failed"
            | "dns_provider_unsupported"
            | "dns_credentials_failed"
            | "dns_provider_timeout"
            | "dns_provider_cancelled"
            | "dns_provider_failed"
            | "dns_cleanup_failed"
    )
}

fn default_renewal_information_status() -> String {
    "not_advertised".into()
}

fn normalized_renewal_information_status(status: &str) -> Option<&'static str> {
    match status {
        "not_advertised" => Some("not_advertised"),
        "pending" => Some("pending"),
        "applied" => Some("applied"),
        "unavailable" => Some("unavailable"),
        "invalid" => Some("invalid"),
        _ => None,
    }
}

fn is_cancelled_error(error: &AcmeManagedError) -> bool {
    matches!(
        error,
        AcmeManagedError::Protocol(AcmeError::Cancelled)
            | AcmeManagedError::DnsProvider(Dns01ProviderError::Cancelled)
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

fn poll_policy(deadline_unix_seconds: u64, cancellation: Option<Dns01Cancellation>) -> PollPolicy {
    PollPolicy {
        max_attempts: 64,
        deadline_unix_seconds,
        initial_delay_seconds: 1,
        max_delay_seconds: 60,
        cancellation,
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

fn restored_next_action(state: &ReconcileState, certificate: &str) -> Option<u64> {
    if state.auto_retry_blocked {
        return None;
    }
    if state.disk_revision == "bootstrap" {
        return Some(0);
    }
    state
        .retry_at_unix_seconds
        .or(state.suggested_renewal_unix_seconds)
        .or_else(|| {
            state.not_before_unix_seconds.and_then(|not_before| {
                state
                    .not_after_unix_seconds
                    .and_then(|not_after| stable_renewal_time(not_before, not_after, certificate))
            })
        })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use openssl::{
        asn1::Asn1Time,
        bn::{BigNum, MsbOption},
        ec::{EcGroup, EcKey},
        hash::MessageDigest,
        nid::Nid,
        pkey::{PKey, Private},
        x509::{
            X509, X509NameBuilder, X509Req,
            extension::{
                AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
                SubjectAlternativeName, SubjectKeyIdentifier,
            },
        },
    };
    use oxiroute_acme::{
        AcmeTransport, ChallengeStore, Dns01Challenge, Dns01Credentials, Dns01Operation,
        Dns01Provider, Dns01ProviderError, Dns01Record, HttpRequest, HttpResponse, RevisionStore,
        StateStore, TransportError,
    };
    use oxiroute_config::{AcmeDns01Config, AcmeKeyType, SelfSignedKeyType};
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
        certificate_names: Vec<String>,
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

    #[derive(Default)]
    struct FakeDnsProvider {
        created: Arc<Mutex<Vec<String>>>,
        propagated: Arc<Mutex<Vec<String>>>,
        cleaned: Arc<Mutex<Vec<String>>>,
        cleanup_fail: Arc<AtomicBool>,
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
                challenge: AcmeChallengeType::Http01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
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
            certificate_names: vec!["proxy.example.test".into()],
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
        assert!(
            requests
                .lock()
                .expect("request log")
                .iter()
                .all(|url| url.starts_with("https://acme.test/"))
        );
    }

    #[test]
    fn tls_alpn01_issues_cleans_up_and_keeps_a_redacted_audit_record() {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let names = vec!["proxy.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed-tls-alpn",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let tls_alpn_challenge_store = TlsAlpnChallengeStore::default();
        let reconciler = AcmeManagedReconciler::new_with_challenge_stores(
            "managed-tls-alpn",
            names,
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::TlsAlpn01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
            },
            revisions.clone(),
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            tls_alpn_challenge_store.clone(),
            None,
            Arc::clone(&active),
        );
        let transport = FakePebbleTransport {
            responses: Arc::new(Mutex::new(VecDeque::from([
                directory_response(),
                nonce_response("nonce-account"),
                account_response(),
                order_response("pending"),
                tls_alpn_authorization_response("pending"),
                challenge_response(),
                tls_alpn_authorization_response("valid"),
                order_response("processing"),
                order_response("valid"),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
            ca: Arc::new(test_ca().expect("CA")),
            certificate: Arc::new(Mutex::new(None)),
            certificate_names: vec!["proxy.example.test".into()],
        };
        let outcome = reconciler
            .renew_with_transport(transport)
            .expect("managed TLS-ALPN-01 issuance");
        assert_eq!(outcome, AcmeManagedOutcome::Activated);
        assert_eq!(reconciler.status().challenge, "tls_alpn01");
        assert!(tls_alpn_challenge_store.is_empty());
        assert!(revisions.load_current("managed-tls-alpn").is_ok());

        let jobs = fs::read_dir(temp.path().join("state/jobs"))
            .expect("redacted job directory")
            .map(|entry| fs::read_to_string(entry.expect("job entry").path()).expect("job"))
            .collect::<Vec<_>>();
        assert!(!jobs.is_empty());
        assert!(jobs.iter().any(|job| job.contains("test")));
        assert!(jobs.iter().all(|job| !job.contains("token-1")));
        assert!(jobs.iter().all(|job| !job.contains("thumbprint")));
    }

    #[test]
    fn tls_alpn01_failure_cleans_up_and_keeps_the_bootstrap_generation() {
        let temp = TempDir::new().expect("state directory");
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let names = vec!["proxy.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed-tls-alpn-failure",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let bootstrap_revision = active.snapshot().metadata().revision.clone();
        let tls_alpn_challenge_store = TlsAlpnChallengeStore::default();
        let reconciler = AcmeManagedReconciler::new_with_challenge_stores(
            "managed-tls-alpn-failure",
            names,
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::TlsAlpn01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
            },
            revisions,
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            tls_alpn_challenge_store.clone(),
            None,
            Arc::clone(&active),
        );
        let transport = FakePebbleTransport {
            responses: Arc::new(Mutex::new(VecDeque::from([
                directory_response(),
                nonce_response("nonce-account"),
                account_response(),
                order_response("pending"),
                tls_alpn_authorization_response("pending"),
                challenge_response(),
                tls_alpn_authorization_response("invalid"),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
            ca: Arc::new(test_ca().expect("CA")),
            certificate: Arc::new(Mutex::new(None)),
            certificate_names: vec!["proxy.example.test".into()],
        };

        let error = reconciler
            .renew_with_transport(transport)
            .expect_err("invalid TLS-ALPN-01 authorization");
        assert!(matches!(error, AcmeManagedError::AuthorizationFailed));
        assert!(tls_alpn_challenge_store.is_empty());
        assert_eq!(
            reconciler
                .active_generation()
                .snapshot()
                .metadata()
                .revision,
            bootstrap_revision
        );
        assert_eq!(reconciler.status().job_status, Some(JobStatus::Failed));
    }

    #[test]
    fn dns01_issues_wildcard_and_cleans_the_exact_provider_record() {
        let temp = TempDir::new().expect("state directory");
        let credentials = temp.path().join("dns-credentials");
        fs::write(&credentials, b"dns-secret").expect("credentials");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))
                .expect("credential permissions");
        }
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let names = vec!["*.example.test".to_owned()];
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed-wildcard",
            &names,
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let provider = Arc::new(FakeDnsProvider::default());
        let created = Arc::clone(&provider.created);
        let propagated = Arc::clone(&provider.propagated);
        let cleaned = Arc::clone(&provider.cleaned);
        let provider: Arc<dyn Dns01Provider> = provider;
        let reconciler = AcmeManagedReconciler::new_with_dns_provider(
            "managed-wildcard",
            names,
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::Dns01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: Some(AcmeDns01Config {
                    provider: "fake".into(),
                    credential_file: credentials,
                    timeout_seconds: 30,
                }),
            },
            revisions.clone(),
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            Some(provider),
            Arc::clone(&active),
        );
        let transport = FakePebbleTransport {
            responses: Arc::new(Mutex::new(VecDeque::from([
                directory_response(),
                nonce_response("nonce-account"),
                account_response(),
                order_response_with_identifier("*.example.test", "pending"),
                dns_authorization_response("pending"),
                challenge_response(),
                dns_authorization_response("valid"),
                order_response_with_identifier("*.example.test", "processing"),
                order_response_with_identifier("*.example.test", "valid"),
            ]))),
            requests: Arc::new(Mutex::new(Vec::new())),
            ca: Arc::new(test_ca().expect("CA")),
            certificate: Arc::new(Mutex::new(None)),
            certificate_names: vec!["*.example.test".into()],
        };
        let outcome = reconciler
            .renew_with_transport(transport)
            .expect("managed DNS-01 issuance");

        assert_eq!(outcome, AcmeManagedOutcome::Activated);
        assert_eq!(reconciler.status().challenge, "dns01");
        assert_eq!(reconciler.status().dns_provider.as_deref(), Some("fake"));
        assert_eq!(
            created.lock().expect("created records").as_slice(),
            ["_acme-challenge.example.test".to_owned()]
        );
        assert_eq!(
            propagated.lock().expect("propagated records").as_slice(),
            ["fake-record-1".to_owned()]
        );
        assert_eq!(
            cleaned.lock().expect("cleaned records").as_slice(),
            ["fake-record-1".to_owned()]
        );
        assert!(revisions.load_current("managed-wildcard").is_ok());
    }

    #[test]
    fn restart_recovers_a_durable_dns_cleanup_journal_and_retries_failures() {
        let temp = TempDir::new().expect("state directory");
        let credentials = temp.path().join("dns-credentials");
        fs::write(&credentials, b"dns-secret").expect("credentials");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))
                .expect("credential permissions");
        }
        let state = Arc::new(StateStore::open(temp.path().join("state")).expect("state"));
        let journal = PersistedDnsCleanup {
            certificate: "managed-recovery".into(),
            provider: "fake".into(),
            identifier: "*.example.test".into(),
            challenge_url: "https://acme.test/challenge/1".into(),
            record_name: "_acme-challenge.example.test".into(),
            record_value: "txt-value".into(),
            provider_record_id: "fake-record-1".into(),
        };
        state
            .write_secret_json("certificates/managed-recovery/dns-cleanup.json", &journal)
            .expect("cleanup journal");
        let revisions = RevisionStore::from_arc(Arc::clone(&state));
        let bootstrap = CertificateGeneration::self_signed_development(
            "managed-recovery",
            &["*.example.test".into()],
            1,
            SelfSignedKeyType::EcdsaP256,
        )
        .expect("bootstrap");
        let active = Arc::new(ActiveCertificateGeneration::new(Arc::new(bootstrap)));
        let provider = Arc::new(FakeDnsProvider::default());
        let cleanup_fail = Arc::clone(&provider.cleanup_fail);
        let cleaned = Arc::clone(&provider.cleaned);
        let reconciler = AcmeManagedReconciler::new_with_dns_provider(
            "managed-recovery",
            vec!["*.example.test".into()],
            AcmeManagedPolicy {
                directory_url: "https://acme.test/directory".into(),
                contacts: vec!["mailto:ops@example.test".into()],
                terms_agreed: true,
                challenge: AcmeChallengeType::Dns01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: Some(AcmeDns01Config {
                    provider: "fake".into(),
                    credential_file: credentials,
                    timeout_seconds: 30,
                }),
            },
            revisions.clone(),
            "bootstrap".into(),
            None,
            None,
            true,
            ChallengeStore::default(),
            Some(provider),
            Arc::clone(&active),
        );
        assert_eq!(
            cleaned.lock().expect("cleaned records").as_slice(),
            ["fake-record-1".to_owned()]
        );
        assert!(
            !temp
                .path()
                .join("state/certificates/managed-recovery/dns-cleanup.json")
                .exists()
        );
        assert_eq!(reconciler.status().dns_cleanup_status, "recovered");
        assert_eq!(reconciler.status().dns_provider_health, Some("healthy"));

        state
            .write_secret_json("certificates/managed-recovery/dns-cleanup.json", &journal)
            .expect("second cleanup journal");
        cleanup_fail.store(true, Ordering::Release);
        assert_eq!(
            reconciler.recover_pending_dns_cleanup(),
            AcmeDnsCleanupRecovery::Deferred
        );
        assert_eq!(reconciler.status().dns_cleanup_status, "failed");
        assert!(
            temp.path()
                .join("state/certificates/managed-recovery/dns-cleanup.json")
                .is_file()
        );
        cleanup_fail.store(false, Ordering::Release);
        assert_eq!(
            reconciler.recover_pending_dns_cleanup(),
            AcmeDnsCleanupRecovery::Recovered
        );
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
                    dns_provider: None,
                    key_type: "ecdsa_p256".into(),
                    next_action_unix_seconds: Some(2_000),
                    retry_at_unix_seconds: Some(2_000),
                    retry_attempt: 2,
                    suggested_renewal_unix_seconds: None,
                    auto_retry_blocked: false,
                    renewal_info_url: None,
                    last_success_unix_seconds: Some(1_000),
                    last_error_code: Some("transport_failed".into()),
                    renewal_information_status: "not_advertised".into(),
                    paused: false,
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
                challenge: AcmeChallengeType::Http01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
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

    #[test]
    fn permanent_and_retryable_protocol_failures_control_automatic_retries() {
        for (problem_type, retryable) in [("rateLimited", true), ("rejectedIdentifier", false)] {
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
                names.clone(),
                AcmeManagedPolicy {
                    directory_url: "https://acme.test/directory".into(),
                    contacts: vec!["mailto:ops@example.test".into()],
                    terms_agreed: true,
                    challenge: AcmeChallengeType::Http01,
                    key_type: AcmeKeyType::EcdsaP256,
                    allowed_dns_suffixes: vec!["example.test".into()],
                    retained_revisions: 3,
                    retention_days: 30,
                    dns01: None,
                },
                revisions,
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
                    problem_response(problem_type),
                ]))),
                requests: Arc::new(Mutex::new(Vec::new())),
                ca: Arc::new(test_ca().expect("CA")),
                certificate: Arc::new(Mutex::new(None)),
                certificate_names: names,
            };

            assert!(matches!(
                reconciler.renew_with_transport(transport),
                Err(AcmeManagedError::Protocol(AcmeError::Problem { .. }))
            ));
            let status = reconciler.status();
            assert_eq!(status.last_error_code.as_deref(), Some("acme_problem"));
            if retryable {
                assert_eq!(status.retry_attempt, 1);
                let next = status.next_action_unix_seconds.expect("retry schedule");
                assert!(!reconciler.renewal_due(next.saturating_sub(1)));
            } else {
                assert_eq!(status.retry_attempt, 0);
                assert_eq!(status.next_action_unix_seconds, None);
                assert!(!reconciler.renewal_due(u64::MAX));
            }
            let renewal = state
                .read_json::<PersistedRenewal>("certificates/managed/renewal.json", MAX_JOB_BYTES)
                .expect("persisted renewal state");
            assert_eq!(renewal.last_error_code.as_deref(), Some("acme_problem"));
            assert_eq!(renewal.auto_retry_blocked, !retryable);
        }
    }

    #[test]
    fn pause_and_resume_restores_the_persisted_ari_schedule() {
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
                    dns_provider: None,
                    key_type: "ecdsa_p256".into(),
                    next_action_unix_seconds: Some(4_000),
                    retry_at_unix_seconds: None,
                    retry_attempt: 0,
                    suggested_renewal_unix_seconds: Some(4_000),
                    auto_retry_blocked: false,
                    renewal_info_url: Some("https://acme.test/acme/renewal-info".into()),
                    last_success_unix_seconds: Some(1_000),
                    last_error_code: None,
                    renewal_information_status: "applied".into(),
                    paused: false,
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
                challenge: AcmeChallengeType::Http01,
                key_type: AcmeKeyType::EcdsaP256,
                allowed_dns_suffixes: vec!["example.test".into()],
                retained_revisions: 3,
                retention_days: 30,
                dns01: None,
            },
            RevisionStore::from_arc(state),
            "revision".into(),
            Some(900),
            Some(10_000),
            false,
            ChallengeStore::default(),
            active,
        );

        assert_eq!(reconciler.status().next_action_unix_seconds, Some(4_000));
        reconciler.pause().expect("pause");
        assert_eq!(reconciler.status().next_action_unix_seconds, None);
        reconciler.resume().expect("resume");
        assert_eq!(reconciler.status().next_action_unix_seconds, Some(4_000));
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

    fn problem_response(problem_type: &str) -> HttpResponse {
        HttpResponse::new(
            400,
            "https://acme.test/acme/new-order",
            format!(r#"{{"type":"urn:ietf:params:acme:error:{problem_type}"}}"#).into_bytes(),
        )
        .with_header("replay-nonce", "nonce-order-problem")
    }

    fn order_response(status: &str) -> HttpResponse {
        order_response_with_identifier("proxy.example.test", status)
    }

    fn order_response_with_identifier(identifier: &str, status: &str) -> HttpResponse {
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

    fn tls_alpn_authorization_response(status: &str) -> HttpResponse {
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

    fn dns_authorization_response(status: &str) -> HttpResponse {
        let challenges = if status == "valid" {
            "[]".to_owned()
        } else {
            r#"[{"type":"dns-01","url":"https://acme.test/acme/challenge/1","token":"token-1"}]"#
                .into()
        };
        let body = format!(
            "{{\"status\":\"{status}\",\"identifier\":{{\"type\":\"dns\",\"value\":\"*.example.test\"}},\"challenges\":{challenges}}}"
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
}
