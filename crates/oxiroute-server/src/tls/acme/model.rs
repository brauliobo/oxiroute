#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) static NEXT_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub(super) const ACME_RETRY_BASE_SECONDS: u64 = 5 * 60;
pub(super) const ACME_RETRY_MAX_SECONDS: u64 = 12 * 60 * 60;

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
    TlsAlpnChallenge(#[source] Box<super::super::TlsAlpnChallengeError>),
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

    pub(super) fn is_retryable(&self) -> bool {
        match self {
            Self::Protocol(error) => error.is_retryable(),
            Self::Publication | Self::DnsCleanup(_) => true,
            Self::Challenge(error) => {
                matches!(
                    error,
                    ChallengeStoreError::DuplicateToken | ChallengeStoreError::CapacityExceeded
                )
            }
            Self::TlsAlpnChallenge(error) => matches!(
                error.as_ref(),
                super::super::TlsAlpnChallengeError::DuplicateIdentifier
                    | super::super::TlsAlpnChallengeError::CapacityExceeded
            ),
            Self::DnsProvider(error) => matches!(
                error,
                Dns01ProviderError::ProviderFailed | Dns01ProviderError::Timeout
            ),
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

pub(super) struct ReconcileState {
    pub(super) disk_revision: String,
    pub(super) not_before_unix_seconds: Option<u64>,
    pub(super) not_after_unix_seconds: Option<u64>,
    pub(super) next_action_unix_seconds: Option<u64>,
    pub(super) retry_attempt: u32,
    pub(super) retry_at_unix_seconds: Option<u64>,
    pub(super) suggested_renewal_unix_seconds: Option<u64>,
    pub(super) auto_retry_blocked: bool,
    pub(super) account_url: Option<String>,
    pub(super) renewal_info_url: Option<String>,
    pub(super) job_status: Option<JobStatus>,
    pub(super) job_id: Option<String>,
    pub(super) paused: bool,
    pub(super) last_success_unix_seconds: Option<u64>,
    pub(super) last_outcome: Option<&'static str>,
    pub(super) last_error_code: Option<String>,
    pub(super) renewal_information_status: &'static str,
    pub(super) dns_provider_health: &'static str,
    pub(super) dns_cleanup_status: &'static str,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PersistedAccount {
    pub(super) directory_url: String,
    pub(super) account: Account,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PersistedRenewal {
    pub(super) certificate: String,
    pub(super) identifiers: Vec<String>,
    pub(super) directory_url: String,
    pub(super) account_url: Option<String>,
    pub(super) authenticator: String,
    #[serde(default)]
    pub(super) dns_provider: Option<String>,
    pub(super) key_type: String,
    pub(super) next_action_unix_seconds: Option<u64>,
    pub(super) retry_at_unix_seconds: Option<u64>,
    pub(super) retry_attempt: u32,
    pub(super) suggested_renewal_unix_seconds: Option<u64>,
    #[serde(default)]
    pub(super) auto_retry_blocked: bool,
    pub(super) renewal_info_url: Option<String>,
    pub(super) last_success_unix_seconds: Option<u64>,
    #[serde(default)]
    pub(super) last_error_code: Option<String>,
    #[serde(default = "default_renewal_information_status")]
    pub(super) renewal_information_status: String,
    #[serde(default)]
    pub(super) paused: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PersistedDnsCleanup {
    pub(super) certificate: String,
    pub(super) provider: String,
    pub(super) identifier: String,
    pub(super) challenge_url: String,
    pub(super) record_name: String,
    pub(super) record_value: String,
    pub(super) provider_record_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcmeDnsCleanupRecovery {
    NotPending,
    Recovered,
    Deferred,
}

#[derive(Default)]
pub(super) struct JobControl {
    pub(super) id: Option<String>,
    pub(super) cancellation: Option<Dns01Cancellation>,
}

/// Owns the managed state root for one certificate and publishes only validated revisions.
pub struct AcmeManagedReconciler {
    pub(super) certificate: String,
    pub(super) declared_dns_names: Vec<String>,
    pub(super) policy: AcmeManagedPolicy,
    pub(super) revisions: RevisionStore,
    pub(super) challenge_store: ChallengeStore,
    pub(super) tls_alpn_challenge_store: TlsAlpnChallengeStore,
    pub(super) dns_provider: Option<Arc<dyn Dns01Provider>>,
    pub(super) active: Arc<ActiveCertificateGeneration>,
    pub(super) job: Mutex<()>,
    pub(super) control: Mutex<JobControl>,
    pub(super) state: Mutex<ReconcileState>,
}

pub(super) type LoadedManagedCertificate = (
    CertificateGeneration,
    RevisionStore,
    String,
    Option<u64>,
    Option<u64>,
    bool,
);
