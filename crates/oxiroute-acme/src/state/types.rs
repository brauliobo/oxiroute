#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum AcmeStateError {
    #[error("ACME state root must be an absolute path")]
    StateRootNotAbsolute,
    #[error("ACME state path contains an unsafe symbolic link or non-directory")]
    UnsafeDirectory,
    #[error("ACME state directory ownership or mode is unsafe")]
    UnsafeDirectoryPermissions,
    #[error("ACME state root is already locked by another process")]
    StateLocked,
    #[error("ACME state lock could not be opened")]
    LockOpen(#[source] io::Error),
    #[error("ACME state lock could not be acquired")]
    LockAcquire,
    #[error("ACME state path is invalid")]
    UnsafePath,
    #[error("ACME state file exceeds the {limit}-byte bound")]
    FileTooLarge { limit: usize },
    #[error("ACME state file is not a regular no-follow file")]
    NotRegularFile,
    #[error("ACME state file could not be opened")]
    FileOpen(#[source] io::Error),
    #[error("ACME state file could not be read")]
    FileRead(#[source] io::Error),
    #[error("ACME state file changed during a stable read")]
    FileChanged,
    #[error("ACME state file could not be written")]
    FileWrite(#[source] io::Error),
    #[error("ACME state file could not be synchronized")]
    FileSync(#[source] io::Error),
    #[error("ACME state revision could not be installed atomically")]
    Rename(#[source] io::Error),
    #[error("ACME state directory could not be synchronized")]
    DirectorySync(#[source] io::Error),
    #[error("ACME state JSON is invalid")]
    Json(#[source] serde_json::Error),
    #[error("ACME state revision is invalid")]
    InvalidRevision,
    #[error("ACME job state is invalid")]
    InvalidJob,
    #[error("ACME job lifecycle transition is invalid")]
    InvalidJobTransition,
    #[error("ACME certificate material is incomplete")]
    IncompleteCertificate,
    #[error("ACME DNS cleanup is unresolved; state deletion is blocked")]
    PendingDnsCleanup,
}

/// Secret bytes are intentionally not serializable or printable.
#[derive(Clone, Default)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    #[must_use]
    pub fn into_bytes(self) -> Zeroizing<Vec<u8>> {
        self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(REDACTED)")
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    WaitingForChallenge,
    Finalizing,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedactedOutcome {
    pub code: String,
    pub message: String,
}

impl RedactedOutcome {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobState {
    pub(super) id: String,
    pub(super) certificate: String,
    pub(super) operation: String,
    #[serde(default)]
    pub(super) correlation_id: Option<String>,
    pub(super) status: JobStatus,
    pub(super) created_at_unix_seconds: u64,
    pub(super) updated_at_unix_seconds: u64,
    pub(super) attempt: u32,
    pub(super) next_action_unix_seconds: Option<u64>,
    pub(super) disk_revision: Option<String>,
    pub(super) active_revision: Option<String>,
    pub(super) last_outcome: Option<RedactedOutcome>,
}

impl JobState {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    #[must_use]
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    #[must_use]
    pub fn status(&self) -> &JobStatus {
        &self.status
    }

    #[must_use]
    pub fn created_at_unix_seconds(&self) -> u64 {
        self.created_at_unix_seconds
    }

    #[must_use]
    pub fn updated_at_unix_seconds(&self) -> u64 {
        self.updated_at_unix_seconds
    }

    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub fn next_action_unix_seconds(&self) -> Option<u64> {
        self.next_action_unix_seconds
    }

    #[must_use]
    pub fn disk_revision(&self) -> Option<&str> {
        self.disk_revision.as_deref()
    }

    #[must_use]
    pub fn active_revision(&self) -> Option<&str> {
        self.active_revision.as_deref()
    }

    #[must_use]
    pub fn last_outcome(&self) -> Option<&RedactedOutcome> {
        self.last_outcome.as_ref()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionMetadata {
    pub certificate: String,
    pub revision: String,
    pub created_at_unix_seconds: u64,
    pub not_before_unix_seconds: Option<u64>,
    pub not_after_unix_seconds: Option<u64>,
    pub issuer: Option<String>,
    pub serial_fingerprint: Option<String>,
    pub key_type: Option<String>,
}

pub struct CertificateMaterial {
    pub certificate_pem: Vec<u8>,
    pub chain_pem: Vec<u8>,
    pub fullchain_pem: Vec<u8>,
    pub private_key_pem: SecretBytes,
    pub metadata: RevisionMetadata,
}

impl fmt::Debug for CertificateMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertificateMaterial")
            .field("certificate_bytes", &self.certificate_pem.len())
            .field("chain_bytes", &self.chain_pem.len())
            .field("fullchain_bytes", &self.fullchain_pem.len())
            .field("private_key", &self.private_key_pem)
            .field("metadata", &self.metadata)
            .finish()
    }
}
