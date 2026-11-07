#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CertbotCertificateDto {
    name: String,
    active_archive_revision: u64,
    active_content_revision: String,
    expires_at: String,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
}

impl From<CertbotCertificateSnapshot> for CertbotCertificateDto {
    fn from(snapshot: CertbotCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            active_archive_revision: snapshot.active_archive_revision,
            active_content_revision: snapshot.active_content_revision,
            expires_at: snapshot.expires_at,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DirectFileCertificateDto {
    name: String,
    active_content_revision: String,
    expires_at: String,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
}

impl From<DirectFileCertificateSnapshot> for DirectFileCertificateDto {
    fn from(snapshot: DirectFileCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            active_content_revision: snapshot.active_content_revision,
            expires_at: snapshot.expires_at,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcmeManagedCertificateDto {
    name: String,
    directory_url: String,
    disk_revision: String,
    active_revision: String,
    expires_at: String,
    not_before_unix_seconds: Option<u64>,
    not_after_unix_seconds: Option<u64>,
    next_action_unix_seconds: Option<u64>,
    last_outcome: Option<String>,
    last_error_code: Option<String>,
    renewal_information_status: String,
    dns_provider: Option<String>,
    dns_provider_deployment: Option<String>,
    dns_provider_health: Option<String>,
    dns_cleanup_status: String,
}

impl From<AcmeManagedCertificateSnapshot> for AcmeManagedCertificateDto {
    fn from(snapshot: AcmeManagedCertificateSnapshot) -> Self {
        Self {
            name: snapshot.name,
            directory_url: snapshot.directory_url,
            disk_revision: snapshot.disk_revision,
            active_revision: snapshot.active_revision,
            expires_at: snapshot.expires_at,
            not_before_unix_seconds: snapshot.not_before_unix_seconds,
            not_after_unix_seconds: snapshot.not_after_unix_seconds,
            next_action_unix_seconds: snapshot.next_action_unix_seconds,
            last_outcome: snapshot.last_outcome,
            last_error_code: snapshot.last_error_code,
            renewal_information_status: snapshot.renewal_information_status,
            dns_provider: snapshot.dns_provider,
            dns_provider_deployment: snapshot.dns_provider_deployment,
            dns_provider_health: snapshot.dns_provider_health,
            dns_cleanup_status: snapshot.dns_cleanup_status,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CertificateWatcherDto {
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

impl From<CertbotWatcherSnapshot> for CertificateWatcherDto {
    fn from(snapshot: CertbotWatcherSnapshot) -> Self {
        Self::from_parts(
            snapshot.health,
            snapshot.coalesced_events,
            snapshot.ignored_access_events,
            snapshot.backend_errors,
            snapshot.watch_recoveries,
            snapshot.watch_refreshes,
            snapshot.rescans,
            snapshot.periodic_rescans,
            snapshot.reconciliation_failures,
        )
    }
}

impl From<DirectFileWatcherSnapshot> for CertificateWatcherDto {
    fn from(snapshot: DirectFileWatcherSnapshot) -> Self {
        Self::from_parts(
            snapshot.health,
            snapshot.coalesced_events,
            snapshot.ignored_access_events,
            snapshot.backend_errors,
            snapshot.watch_recoveries,
            snapshot.watch_refreshes,
            snapshot.rescans,
            snapshot.periodic_rescans,
            snapshot.reconciliation_failures,
        )
    }
}

impl CertificateWatcherDto {
    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        health: CertbotWatcherHealth,
        coalesced_events: u64,
        ignored_access_events: u64,
        backend_errors: u64,
        watch_recoveries: u64,
        watch_refreshes: u64,
        rescans: u64,
        periodic_rescans: u64,
        reconciliation_failures: u64,
    ) -> Self {
        Self {
            health: health.into(),
            coalesced_events: coalesced_events.into(),
            ignored_access_events: ignored_access_events.into(),
            backend_errors: backend_errors.into(),
            watch_recoveries: watch_recoveries.into(),
            watch_refreshes: watch_refreshes.into(),
            rescans: rescans.into(),
            periodic_rescans: periodic_rescans.into(),
            reconciliation_failures: reconciliation_failures.into(),
        }
    }
}
