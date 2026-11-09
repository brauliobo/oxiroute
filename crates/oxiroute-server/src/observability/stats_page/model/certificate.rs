use schemars::JsonSchema;
use serde::Serialize;

use super::{CertbotWatcherHealth, CertificateSource, Counter};

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CertificateStats {
    pub(crate) name: String,
    pub(crate) source: CertificateSource,
    /// The leaf expiry as the TLS library prints it, in GMT.
    pub(crate) expires_at: String,
    /// The same instant as Unix seconds; absent when the printed form could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_after_unix_seconds: Option<u64>,
    /// When the ACME reconciler next acts on the certificate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_action_unix_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error_code: Option<String>,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CertificateWatcherStats {
    pub(crate) source: CertificateSource,
    pub(crate) health: CertbotWatcherHealth,
    pub(crate) backend_errors: Counter,
    pub(crate) reconciliation_failures: Counter,
}
