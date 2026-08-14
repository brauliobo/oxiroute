use std::net::SocketAddr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AdministrativeState, HealthOverride,
    config_coordinator::{
        AuthoredRevision, ConfigDiagnostic, ConfigDiagnosticSeverity, ConfigDiagnosticStage,
        EffectiveRevision,
    },
};

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ListenerStateRequest {
    pub(crate) listeners: Vec<String>,
    #[schemars(with = "AdministrativeStateSchema")]
    pub(crate) state: AdministrativeState,
    pub(crate) expected_active_revision: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PoolStateRequest {
    pub(crate) pools: Vec<String>,
    #[schemars(with = "AdministrativeStateSchema")]
    pub(crate) state: AdministrativeState,
    pub(crate) expected_active_revision: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerTarget {
    pub(crate) pool: String,
    pub(crate) server: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerChange {
    pub(crate) targets: Vec<ServerTarget>,
    pub(crate) expected_active_revision: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerStateRequest {
    #[serde(flatten)]
    pub(crate) change: ServerChange,
    #[schemars(with = "AdministrativeStateSchema")]
    pub(crate) state: AdministrativeState,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerHealthRequest {
    #[serde(flatten)]
    pub(crate) change: ServerChange,
    #[schemars(with = "HealthOverrideSchema")]
    pub(crate) health: HealthOverride,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerChecksRequest {
    #[serde(flatten)]
    pub(crate) change: ServerChange,
    pub(crate) enabled: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerCapacityRequest {
    #[serde(flatten)]
    pub(crate) change: ServerChange,
    pub(crate) max_connections: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevisionRequest {
    pub(crate) expected_active_revision: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DrainRequest {
    pub(crate) expected_active_revision: String,
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
}

#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum AdministrativeStateSchema {
    Ready,
    Drain,
    Maintenance,
}

#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum HealthOverrideSchema {
    Auto,
    Up,
    Down,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct MutationResponse {
    outcome: &'static str,
    changed: usize,
}

impl MutationResponse {
    pub(crate) const fn applied(changed: usize) -> Self {
        Self {
            outcome: "applied",
            changed,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DnsRefreshResponse {
    outcome: DnsRefreshOutcome,
    atomic: bool,
    servers: Vec<DnsRefreshServer>,
}

impl DnsRefreshResponse {
    pub(crate) fn new(failed: bool, servers: Vec<DnsRefreshServer>) -> Self {
        Self {
            outcome: if failed {
                DnsRefreshOutcome::PartiallyRefreshed
            } else {
                DnsRefreshOutcome::Refreshed
            },
            atomic: false,
            servers,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum DnsRefreshOutcome {
    Refreshed,
    PartiallyRefreshed,
}

#[derive(JsonSchema, Serialize)]
#[serde(untagged)]
pub(crate) enum DnsRefreshServer {
    Refreshed(DnsRefreshedServer),
    Failed(DnsFailedServer),
}

impl DnsRefreshServer {
    pub(crate) fn refreshed(pool: String, server: String, addresses: &[SocketAddr]) -> Self {
        Self::Refreshed(DnsRefreshedServer {
            pool,
            server,
            outcome: DnsServerOutcome::Refreshed,
            addresses: addresses.iter().map(ToString::to_string).collect(),
        })
    }

    pub(crate) fn failed(pool: String, server: String) -> Self {
        Self::Failed(DnsFailedServer {
            pool,
            server,
            outcome: DnsServerOutcome::Failed,
            error: DnsRefreshError {
                code: "dns_refresh_failed",
            },
        })
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct DnsRefreshedServer {
    pool: String,
    server: String,
    outcome: DnsServerOutcome,
    addresses: Vec<String>,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct DnsFailedServer {
    pool: String,
    server: String,
    outcome: DnsServerOutcome,
    error: DnsRefreshError,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum DnsServerOutcome {
    Refreshed,
    Failed,
}

#[derive(JsonSchema, Serialize)]
struct DnsRefreshError {
    code: &'static str,
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigRejectedResponse {
    error: ConfigRejectedError,
    disk_revision: Option<String>,
    active_revision: Option<String>,
    diagnostics: Vec<ConfigDiagnosticDto>,
}

impl ConfigRejectedResponse {
    pub(crate) fn new(
        disk_revision: Option<AuthoredRevision>,
        active_revision: Option<EffectiveRevision>,
        diagnostics: Vec<ConfigDiagnostic>,
    ) -> Self {
        Self {
            error: ConfigRejectedError {
                code: "config_rejected",
                message: "persisted configuration is invalid",
            },
            disk_revision: disk_revision.map(|value| value.as_str().to_owned()),
            active_revision: active_revision.map(|value| value.as_str().to_owned()),
            diagnostics: diagnostics.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
struct ConfigRejectedError {
    code: &'static str,
    message: &'static str,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct ConfigDiagnosticDto {
    code: &'static str,
    severity: ConfigDiagnosticSeverityDto,
    stage: ConfigDiagnosticStageDto,
    message: &'static str,
}

impl From<ConfigDiagnostic> for ConfigDiagnosticDto {
    fn from(diagnostic: ConfigDiagnostic) -> Self {
        Self {
            code: diagnostic.code,
            severity: diagnostic.severity.into(),
            stage: diagnostic.stage.into(),
            message: diagnostic.message,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfigDiagnosticSeverityDto {
    Error,
    Warning,
}

impl From<ConfigDiagnosticSeverity> for ConfigDiagnosticSeverityDto {
    fn from(severity: ConfigDiagnosticSeverity) -> Self {
        match severity {
            ConfigDiagnosticSeverity::Error => Self::Error,
            ConfigDiagnosticSeverity::Warning => Self::Warning,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfigDiagnosticStageDto {
    Read,
    Parse,
    Validation,
    Render,
    Conflict,
    Write,
    Sync,
    Rollback,
}

impl From<ConfigDiagnosticStage> for ConfigDiagnosticStageDto {
    fn from(stage: ConfigDiagnosticStage) -> Self {
        match stage {
            ConfigDiagnosticStage::Read => Self::Read,
            ConfigDiagnosticStage::Parse => Self::Parse,
            ConfigDiagnosticStage::Validation => Self::Validation,
            ConfigDiagnosticStage::Render => Self::Render,
            ConfigDiagnosticStage::Conflict => Self::Conflict,
            ConfigDiagnosticStage::Write => Self::Write,
            ConfigDiagnosticStage::Sync => Self::Sync,
            ConfigDiagnosticStage::Rollback => Self::Rollback,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerationActionResponse {
    outcome: &'static str,
    candidate_revision: String,
}

impl GenerationActionResponse {
    pub(crate) fn startup(candidate_revision: &EffectiveRevision) -> Self {
        Self {
            outcome: "startup_requested",
            candidate_revision: candidate_revision.as_str().to_owned(),
        }
    }

    pub(crate) fn rollback(candidate_revision: &EffectiveRevision) -> Self {
        Self {
            outcome: "rollback_startup_requested",
            candidate_revision: candidate_revision.as_str().to_owned(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DrainResponse {
    outcome: DrainOutcome,
    active_references: u64,
}

impl DrainResponse {
    pub(crate) const fn new(drained: bool, active_references: u64) -> Self {
        Self {
            outcome: if drained {
                DrainOutcome::Drained
            } else {
                DrainOutcome::Draining
            },
            active_references,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum DrainOutcome {
    Drained,
    Draining,
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct ProcessMutationResponse {
    outcome: ProcessMutationOutcome,
}

impl ProcessMutationResponse {
    pub(crate) const fn draining() -> Self {
        Self {
            outcome: ProcessMutationOutcome::Draining,
        }
    }

    pub(crate) const fn shutdown_requested() -> Self {
        Self {
            outcome: ProcessMutationOutcome::ShutdownRequested,
        }
    }
}

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProcessMutationOutcome {
    Draining,
    ShutdownRequested,
}

#[cfg(test)]
mod tests {
    use schemars::generate::SchemaSettings;
    use serde_json::json;

    use super::*;

    #[test]
    fn mutation_request_schemas_preserve_casing_enums_and_nullability() {
        let generator = SchemaSettings::default().for_deserialize().into_generator();
        let listener =
            serde_json::to_value(generator.into_root_schema_for::<ListenerStateRequest>())
                .expect("listener request schema");
        let generator = SchemaSettings::default().for_deserialize().into_generator();
        let capacity =
            serde_json::to_value(generator.into_root_schema_for::<ServerCapacityRequest>())
                .expect("capacity request schema");

        assert_eq!(
            listener["properties"]["state"]["enum"],
            json!(["ready", "drain", "maintenance"])
        );
        assert!(
            listener["properties"]
                .get("expectedActiveRevision")
                .is_some()
        );
        assert_eq!(
            capacity["properties"]["maxConnections"]["type"],
            json!(["integer", "null"])
        );
        assert_eq!(listener["additionalProperties"], false);
    }

    #[test]
    fn mutation_responses_preserve_omitted_and_null_fields() {
        let refreshed = DnsRefreshServer::refreshed(
            "pool".into(),
            "server".into(),
            &["127.0.0.1:443".parse().expect("address")],
        );
        let failed = DnsRefreshServer::failed("pool".into(), "missing".into());
        assert_eq!(
            serde_json::to_value(DnsRefreshResponse::new(true, vec![refreshed, failed]))
                .expect("DNS response"),
            json!({
                "outcome": "partially_refreshed",
                "atomic": false,
                "servers": [
                    { "pool": "pool", "server": "server", "outcome": "refreshed", "addresses": ["127.0.0.1:443"] },
                    { "pool": "pool", "server": "missing", "outcome": "failed", "error": { "code": "dns_refresh_failed" } },
                ],
            })
        );
    }
}
