use std::str::FromStr;

use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use oxiroute_config::{ConfigDraft, ValidatedConfig};
use pingora::protocols::http::ServerSession;
use serde_json::{Value, json};

use crate::{
    CertbotWatcherConfig, FileWatcherConfig, RuntimeMode, RuntimePlan,
    config::coordinator::{AuthoredRevision, ConfigDiagnostic},
    generation::restart::StartupOnlyChange,
    listener::inventory::ListenerChangeSummary,
    rtmp::api::{ApiResponse, MAX_CONFIG_REQUEST_BYTES, dto::ConfigRequest, ui::UiAssets},
    service_plan::validation_plan,
};

const IF_CONFIG_REVISION: &str = "if-config-revision";

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(super) enum ConfigPreparationError {
    #[error("runtime configuration preflight failed")]
    Runtime,
    #[error("configured management UI assets are not readable")]
    UiAssets,
    #[error("Certbot watcher prerequisites are unavailable")]
    CertbotWatcher,
    #[error("direct-file watcher prerequisites are unavailable")]
    DirectFileWatcher,
}

pub(super) fn parse_config_request(body: &[u8]) -> Result<ConfigDraft, ApiResponse> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        ApiResponse::error(
            400,
            "malformed_json",
            "request body is not well-formed JSON",
        )
    })?;
    serde_json::from_value::<ConfigRequest>(value)
        .map(ConfigRequest::into_config)
        .map_err(|error| {
            ApiResponse::json(
                422,
                &json!({
                    "diagnostics": [{
                        "code": "E_INVALID_FIELD",
                        "severity": "error",
                        "stage": "parse",
                        "path": "/config",
                        "message": "the config field does not match the canonical configuration schema",
                        "detail": crate::config::coordinator::without_values(&error.to_string()),
                    }],
                }),
            )
        })
}

pub(super) fn prepare_config(
    config: &ValidatedConfig,
) -> Result<RuntimePlan, ConfigPreparationError> {
    let plan = validation_plan(config).map_err(|_| ConfigPreparationError::Runtime)?;
    let acquired = crate::service_plan::validate_runtime_services(&plan)
        .map_err(|_| ConfigPreparationError::Runtime)?;
    if let Some(ui_dir) = config
        .as_draft()
        .management
        .as_ref()
        .and_then(|management| management.ui_dir.as_deref())
    {
        UiAssets::load(ui_dir).map_err(|_| ConfigPreparationError::UiAssets)?;
    }
    acquired
        .tls()
        .check_certbot_watcher(CertbotWatcherConfig::default())
        .map_err(|_| ConfigPreparationError::CertbotWatcher)?;
    acquired
        .tls()
        .check_file_watcher(FileWatcherConfig::default())
        .map_err(|_| ConfigPreparationError::DirectFileWatcher)?;
    Ok(plan)
}

pub(super) fn preparation_diagnostic(error: ConfigPreparationError) -> Value {
    match error {
        ConfigPreparationError::Runtime => json!({
            "code": "E_RUNTIME_PREPARE",
            "severity": "error",
            "stage": "validation",
            "path": "/config",
            "message": "the candidate cannot be prepared as a complete runtime generation",
        }),
        ConfigPreparationError::UiAssets => json!({
            "code": "E_UI_ASSETS",
            "severity": "error",
            "stage": "validation",
            "path": "/config/management/ui_dir",
            "message": "the configured management UI assets are not readable",
        }),
        ConfigPreparationError::CertbotWatcher => json!({
            "code": "E_CERTBOT_WATCHER",
            "severity": "error",
            "stage": "validation",
            "path": "/config/certificates",
            "message": "the configured Certbot watcher prerequisites are unavailable",
        }),
        ConfigPreparationError::DirectFileWatcher => json!({
            "code": "E_DIRECT_FILE_WATCHER",
            "severity": "error",
            "stage": "validation",
            "path": "/config/certificates",
            "message": "the configured direct-file watcher prerequisites are unavailable",
        }),
    }
}

pub(super) fn activation_pending_diagnostic() -> Value {
    json!({
        "code": "I_ACTIVATION_PENDING",
        "severity": "warning",
        "stage": "activation",
        "message": "configuration was saved and queued for generation activation",
    })
}

pub(super) fn restart_required_diagnostic(change: &StartupOnlyChange) -> Value {
    json!({
        "code": "I_RESTART_REQUIRED",
        "severity": "warning",
        "stage": "activation",
        "path": change.path,
        "message": change.summary,
        "reason": change.code,
    })
}

pub(super) fn listener_changes_diagnostic(
    mode: RuntimeMode,
    changes: &ListenerChangeSummary,
) -> Value {
    let binder = match mode {
        RuntimeMode::Supervised => "the supervising master binds",
        RuntimeMode::Direct => "the process binds",
    };
    let mut clauses = Vec::with_capacity(2);
    if !changes.added.is_empty() {
        clauses.push(format!(
            "adds {} ({binder} them before traffic switches)",
            changes.added.join(", ")
        ));
    }
    if !changes.removed.is_empty() {
        clauses.push(format!(
            "removes {} (they stop accepting once the new revision is active and close when their connections finish)",
            changes.removed.join(", ")
        ));
    }
    let message = format!(
        "activation {}; every other listener keeps serving without interruption",
        clauses.join(" and ")
    );
    json!({
        "code": "I_LISTENER_CHANGES",
        "severity": "warning",
        "stage": "activation",
        "path": "/config/listeners",
        "message": message,
        "listeners": {
            "added": changes.added,
            "removed": changes.removed,
            "kept": changes.kept,
        },
    })
}

pub(super) fn diagnostics_json(diagnostics: &[ConfigDiagnostic], as_warnings: bool) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "severity": if as_warnings { "warning" } else {
                    match diagnostic.severity {
                        crate::config::coordinator::ConfigDiagnosticSeverity::Error => "error",
                        crate::config::coordinator::ConfigDiagnosticSeverity::Warning => "warning",
                    }
                },
                "stage": diagnostic.stage,
                "message": diagnostic.message,
            })
        })
        .collect()
}

pub(super) fn config_revision_header(
    session: &ServerSession,
) -> Result<AuthoredRevision, ApiResponse> {
    let mut values = session
        .req_header()
        .headers
        .get_all(IF_CONFIG_REVISION)
        .iter();
    let Some(value) = values.next() else {
        return Err(ApiResponse::error(
            428,
            "precondition_required",
            "If-Config-Revision header is required for configuration writes",
        ));
    };
    if values.next().is_some() {
        return Err(ApiResponse::error(
            400,
            "invalid_config_revision",
            "exactly one If-Config-Revision header is required",
        ));
    }
    value
        .to_str()
        .ok()
        .and_then(|value| AuthoredRevision::from_str(&value.to_ascii_lowercase()).ok())
        .ok_or_else(|| {
            ApiResponse::error(
                400,
                "invalid_config_revision",
                "If-Config-Revision must contain one raw 64-hex configuration revision",
            )
        })
}

pub(super) fn require_json_content_type(session: &ServerSession) -> Result<(), ApiResponse> {
    let mut values = session.req_header().headers.get_all(CONTENT_TYPE).iter();
    let valid = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"));
    if valid && values.next().is_none() {
        Ok(())
    } else {
        Err(ApiResponse::error(
            415,
            "unsupported_media_type",
            "Content-Type must be application/json",
        ))
    }
}

pub(crate) async fn read_config_body(session: &mut ServerSession) -> Result<Vec<u8>, ApiResponse> {
    let mut content_lengths = session.req_header().headers.get_all(CONTENT_LENGTH).iter();
    let declared_length = content_lengths
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(())
        })
        .transpose()
        .map_err(|()| {
            ApiResponse::error(
                400,
                "invalid_content_length",
                "Content-Length must be a nonnegative decimal integer",
            )
        })?;
    if content_lengths.next().is_some() {
        return Err(ApiResponse::error(
            400,
            "invalid_content_length",
            "exactly one Content-Length value is allowed",
        ));
    }
    let max_request_bytes =
        u64::try_from(MAX_CONFIG_REQUEST_BYTES).expect("configuration request limit fits u64");
    if declared_length.is_some_and(|length| length > max_request_bytes) {
        return Err(config_body_too_large());
    }
    let capacity = declared_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0);
    let mut body = Vec::with_capacity(capacity);
    loop {
        let chunk = session.read_request_body().await.map_err(|_| {
            ApiResponse::error(
                400,
                "invalid_request_body",
                "request body could not be read",
            )
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.len() > MAX_CONFIG_REQUEST_BYTES.saturating_sub(body.len()) {
            return Err(config_body_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn config_body_too_large() -> ApiResponse {
    ApiResponse::error(
        413,
        "config_body_too_large",
        format!("configuration request body exceeds the {MAX_CONFIG_REQUEST_BYTES}-byte limit"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_preparation_diagnostic_contract_is_fixed_and_redacted() {
        let diagnostic = preparation_diagnostic(ConfigPreparationError::Runtime);

        assert_eq!(diagnostic["code"], "E_RUNTIME_PREPARE");
        assert_eq!(diagnostic["severity"], "error");
        assert_eq!(diagnostic["stage"], "validation");
        assert_eq!(diagnostic["path"], "/config");
        let wire = diagnostic.to_string();
        for secret in [
            "/secret/tenant/key.pem",
            "token=super-secret",
            "https://user:password@example.test/private",
        ] {
            assert!(!wire.contains(secret));
        }

        let response = ApiResponse::json(422, &json!({ "diagnostics": [diagnostic] }));
        assert_eq!(response.status, 422);
    }
}
