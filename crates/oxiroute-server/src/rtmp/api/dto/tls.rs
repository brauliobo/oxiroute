use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DecimalCounter;
use crate::{
    AcmeManagedStatus, CertbotCertificateSnapshot, CertbotReconcilerStatus, CertbotWatcherHealth,
    CertbotWatcherSnapshot, DirectFileCertificateSnapshot,
};

enum OptionalNullable<T> {
    Omitted,
    Present(Option<T>),
}

impl<T> OptionalNullable<T> {
    const fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted)
    }
}

impl<T: Serialize> Serialize for OptionalNullable<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Omitted => serializer.serialize_none(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TlsRequest {
    pub(crate) expected_active_revision: String,
    #[serde(default)]
    pub(crate) certificate: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TlsRevokeRequest {
    pub(crate) expected_active_revision: String,
    pub(crate) certificate: String,
    #[serde(default)]
    pub(crate) reason: Option<u8>,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct TlsInventoryResponse {
    certificates: Vec<TlsCertificateDto>,
    watcher: Option<CertbotWatcherDto>,
}

impl TlsInventoryResponse {
    pub(crate) fn new(
        certificates: Vec<TlsCertificateDto>,
        watcher: Option<CertbotWatcherSnapshot>,
    ) -> Self {
        Self {
            certificates,
            watcher: watcher.map(Into::into),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsCertificateDto {
    name: String,
    dns_names: Vec<String>,
    source: TlsCertificateSource,
    development_only: bool,
    status: Option<TlsCertificateStatus>,
}

impl TlsCertificateDto {
    pub(crate) fn files(
        name: String,
        dns_names: Vec<String>,
        status: Option<DirectFileCertificateSnapshot>,
    ) -> Self {
        Self {
            name,
            dns_names,
            source: TlsCertificateSource::Files,
            development_only: false,
            status: status.map(|status| TlsCertificateStatus::Material(status.into())),
        }
    }

    pub(crate) fn certbot(
        name: String,
        dns_names: Vec<String>,
        status: Option<CertbotCertificateSnapshot>,
    ) -> Self {
        Self {
            name,
            dns_names,
            source: TlsCertificateSource::Certbot,
            development_only: false,
            status: status.map(|status| TlsCertificateStatus::Material(status.into())),
        }
    }

    pub(crate) fn managed(
        name: String,
        dns_names: Vec<String>,
        status: Option<AcmeManagedStatus>,
    ) -> Self {
        Self {
            name,
            dns_names,
            source: TlsCertificateSource::AcmeManaged,
            development_only: false,
            status: status.map(|status| TlsCertificateStatus::Managed(Box::new(status.into()))),
        }
    }

    pub(crate) fn self_signed(
        name: String,
        dns_names: Vec<String>,
        active_content_revision: Option<String>,
        expires_at: Option<String>,
    ) -> Self {
        let status = active_content_revision
            .zip(expires_at)
            .map(|(revision, expires_at)| {
                TlsCertificateStatus::Material(TlsMaterialStatusDto {
                    active_content_revision: revision,
                    expires_at,
                    active_archive_revision: None,
                    last_outcome: OptionalNullable::Omitted,
                    last_error_code: OptionalNullable::Omitted,
                })
            });
        Self {
            name,
            dns_names,
            source: TlsCertificateSource::SelfSignedDevelopment,
            development_only: true,
            status,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum TlsCertificateSource {
    Files,
    Certbot,
    AcmeManaged,
    SelfSignedDevelopment,
}

#[derive(JsonSchema, Serialize)]
#[serde(untagged)]
enum TlsCertificateStatus {
    Material(TlsMaterialStatusDto),
    Managed(Box<TlsManagedCertificateStatusDto>),
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsMaterialStatusDto {
    active_content_revision: String,
    expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_archive_revision: Option<u64>,
    #[serde(skip_serializing_if = "OptionalNullable::is_omitted")]
    #[schemars(with = "Option<String>")]
    last_outcome: OptionalNullable<String>,
    #[serde(skip_serializing_if = "OptionalNullable::is_omitted")]
    #[schemars(with = "Option<String>")]
    last_error_code: OptionalNullable<String>,
}

impl From<DirectFileCertificateSnapshot> for TlsMaterialStatusDto {
    fn from(status: DirectFileCertificateSnapshot) -> Self {
        Self {
            active_content_revision: status.active_content_revision,
            expires_at: status.expires_at,
            active_archive_revision: None,
            last_outcome: OptionalNullable::Present(status.last_outcome),
            last_error_code: OptionalNullable::Present(status.last_error_code),
        }
    }
}

impl From<CertbotCertificateSnapshot> for TlsMaterialStatusDto {
    fn from(status: CertbotCertificateSnapshot) -> Self {
        Self {
            active_content_revision: status.active_content_revision,
            expires_at: status.expires_at,
            active_archive_revision: Some(status.active_archive_revision),
            last_outcome: OptionalNullable::Present(status.last_outcome),
            last_error_code: OptionalNullable::Present(status.last_error_code),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsManagedCertificateStatusDto {
    certificate: String,
    directory_url: String,
    #[schemars(with = "AcmeChallengeSchema")]
    challenge: String,
    dns_provider: Option<String>,
    key_type: String,
    allowed_dns_suffixes: Vec<String>,
    disk_revision: String,
    active_revision: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    not_after: String,
    job_status: Option<JobStatusDto>,
    job_id: Option<String>,
    paused: bool,
    retained_revisions: u32,
    retention_days: u32,
    retry_attempt: u32,
    last_success_unix_seconds: Option<u64>,
    last_outcome: Option<&'static str>,
    last_error_code: Option<String>,
    renewal_information_status: &'static str,
    dns_provider_deployment: Option<&'static str>,
    dns_provider_health: Option<&'static str>,
    dns_cleanup_status: &'static str,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
#[allow(dead_code)]
enum AcmeChallengeSchema {
    #[schemars(rename = "http01")]
    Http01,
    #[schemars(rename = "dns01")]
    Dns01,
    TlsAlpn01,
}

impl From<AcmeManagedStatus> for TlsManagedCertificateStatusDto {
    fn from(status: AcmeManagedStatus) -> Self {
        Self {
            certificate: status.certificate,
            directory_url: status.directory_url,
            challenge: status.challenge,
            dns_provider: status.dns_provider,
            key_type: status.key_type,
            allowed_dns_suffixes: status.allowed_dns_suffixes,
            disk_revision: status.disk_revision,
            active_revision: status.active_revision,
            not_before_unix_seconds: status.not_before_unix_seconds,
            not_after_unix_seconds: status.not_after_unix_seconds,
            next_action_unix_seconds: status.next_action_unix_seconds,
            not_after: status.not_after,
            job_status: status.job_status.map(Into::into),
            job_id: status.job_id,
            paused: status.paused,
            retained_revisions: status.retained_revisions,
            retention_days: status.retention_days,
            retry_attempt: status.retry_attempt,
            last_success_unix_seconds: status.last_success_unix_seconds,
            last_outcome: status.last_outcome,
            last_error_code: status.last_error_code,
            renewal_information_status: status.renewal_information_status,
            dns_provider_deployment: status.dns_provider_deployment,
            dns_provider_health: status.dns_provider_health,
            dns_cleanup_status: status.dns_cleanup_status,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobStatusDto {
    Queued,
    Running,
    WaitingForChallenge,
    Finalizing,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl From<oxiroute_acme::JobStatus> for JobStatusDto {
    fn from(status: oxiroute_acme::JobStatus) -> Self {
        match status {
            oxiroute_acme::JobStatus::Queued => Self::Queued,
            oxiroute_acme::JobStatus::Running => Self::Running,
            oxiroute_acme::JobStatus::WaitingForChallenge => Self::WaitingForChallenge,
            oxiroute_acme::JobStatus::Finalizing => Self::Finalizing,
            oxiroute_acme::JobStatus::Paused => Self::Paused,
            oxiroute_acme::JobStatus::Succeeded => Self::Succeeded,
            oxiroute_acme::JobStatus::Failed => Self::Failed,
            oxiroute_acme::JobStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct CertbotWatcherDto {
    health: CertbotWatcherHealthDto,
    coalesced_events: DecimalCounter,
    ignored_access_events: DecimalCounter,
    backend_errors: DecimalCounter,
    watch_recoveries: DecimalCounter,
    watch_refreshes: DecimalCounter,
    rescans: DecimalCounter,
    periodic_rescans: DecimalCounter,
    reconciliation_failures: DecimalCounter,
}

impl From<CertbotWatcherSnapshot> for CertbotWatcherDto {
    fn from(status: CertbotWatcherSnapshot) -> Self {
        Self {
            health: status.health.into(),
            coalesced_events: status.coalesced_events.into(),
            ignored_access_events: status.ignored_access_events.into(),
            backend_errors: status.backend_errors.into(),
            watch_recoveries: status.watch_recoveries.into(),
            watch_refreshes: status.watch_refreshes.into(),
            rescans: status.rescans.into(),
            periodic_rescans: status.periodic_rescans.into(),
            reconciliation_failures: status.reconciliation_failures.into(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum CertbotWatcherHealthDto {
    Healthy,
    Degraded,
    Stopped,
}

impl From<CertbotWatcherHealth> for CertbotWatcherHealthDto {
    fn from(health: CertbotWatcherHealth) -> Self {
        match health {
            CertbotWatcherHealth::Healthy => Self::Healthy,
            CertbotWatcherHealth::Degraded => Self::Degraded,
            CertbotWatcherHealth::Stopped => Self::Stopped,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct TlsReconcileResponse {
    outcomes: Vec<TlsReconcileOutcome>,
}

impl TlsReconcileResponse {
    pub(crate) const fn new(outcomes: Vec<TlsReconcileOutcome>) -> Self {
        Self { outcomes }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsReconcileOutcome {
    certificate: String,
    outcome: String,
    #[serde(skip_serializing_if = "OptionalNullable::is_omitted")]
    #[schemars(with = "Option<String>")]
    previous_archive_revision: OptionalNullable<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_revision: Option<String>,
}

impl TlsReconcileOutcome {
    pub(crate) fn certbot(
        status: CertbotReconcilerStatus,
        outcome: &str,
        previous_archive_revision: Option<String>,
        archive_revision: String,
    ) -> Self {
        Self {
            certificate: status.certificate,
            outcome: outcome.to_owned(),
            previous_archive_revision: OptionalNullable::Present(previous_archive_revision),
            archive_revision: Some(archive_revision),
            disk_revision: None,
            active_revision: None,
        }
    }

    pub(crate) fn managed(status: AcmeManagedStatus, outcome: &str) -> Self {
        Self {
            certificate: status.certificate,
            outcome: outcome.to_owned(),
            previous_archive_revision: OptionalNullable::Omitted,
            archive_revision: None,
            disk_revision: Some(status.disk_revision),
            active_revision: Some(status.active_revision),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsRenewResponse {
    certificate: String,
    outcome: String,
    disk_revision: String,
    active_revision: String,
}

impl TlsRenewResponse {
    pub(crate) fn new(status: AcmeManagedStatus, outcome: &str) -> Self {
        Self {
            certificate: status.certificate,
            outcome: outcome.to_owned(),
            disk_revision: status.disk_revision,
            active_revision: status.active_revision,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsActionResponse {
    certificate: String,
    outcome: String,
    job_id: Option<String>,
}

impl TlsActionResponse {
    pub(crate) fn new(certificate: String, outcome: &str, job_id: Option<String>) -> Self {
        Self {
            certificate,
            outcome: outcome.to_owned(),
            job_id,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlsJobControlResponse {
    certificate: String,
    outcome: &'static str,
    #[serde(skip_serializing_if = "OptionalNullable::is_omitted")]
    #[schemars(with = "Option<String>")]
    job_id: OptionalNullable<String>,
}

impl TlsJobControlResponse {
    pub(crate) fn cancellation(certificate: String, job_id: String) -> Self {
        Self {
            certificate,
            outcome: "cancellation_requested",
            job_id: OptionalNullable::Present(Some(job_id)),
        }
    }

    pub(crate) fn paused(certificate: String, job_id: Option<String>) -> Self {
        Self {
            certificate,
            outcome: "paused",
            job_id: OptionalNullable::Present(job_id),
        }
    }

    pub(crate) fn resumed(certificate: String) -> Self {
        Self {
            certificate,
            outcome: "resumed",
            job_id: OptionalNullable::Omitted,
        }
    }
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::json;

    use super::*;

    #[test]
    fn tls_action_dtos_preserve_null_and_omitted_job_ids() {
        assert_eq!(
            serde_json::to_value(TlsActionResponse::new("edge".into(), "deleted", None))
                .expect("action response"),
            json!({ "certificate": "edge", "outcome": "deleted", "jobId": null })
        );
        assert_eq!(
            serde_json::to_value(TlsJobControlResponse::resumed("edge".into()))
                .expect("resume response"),
            json!({ "certificate": "edge", "outcome": "resumed" })
        );
    }

    #[test]
    fn tls_inventory_schema_is_an_explicit_secret_safe_allowlist() {
        let generator = SchemaSettings::default().for_serialize().into_generator();
        let schema =
            serde_json::to_string(&generator.into_root_schema_for::<TlsInventoryResponse>())
                .expect("TLS schema");

        for secret in ["privateKey", "contacts", "credentials", "token", "password"] {
            assert!(!schema.contains(secret), "TLS schema exposed {secret}");
        }
        assert!(schema.contains("dnsCleanupStatus"));
        assert!(schema.contains("reconciliationFailures"));
    }
}
