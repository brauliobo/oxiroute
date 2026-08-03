use std::{io, path::Path, str::FromStr};

use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use oxiroute_config::Config;
use oxiroute_config_source::ConfigFormat;
use pingora::protocols::http::ServerSession;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ApiResponse, MAX_CONFIG_REQUEST_BYTES, observability::candidate_topology,
    response::system_time_ms, ui::UiAssets,
};
use crate::{
    CertbotWatcherConfig, FileWatcherConfig, GenerationManager, RuntimePlan,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigConflict, ConfigDiagnostic, ConfigLoadOutcome,
        ConfigRevision, ConfigSaveFailure, ConfigSaveOutcome, ConfigValidationOutcome,
        ValidatedConfigDraft,
    },
    listener_reservation::unix_listener_mode_change_requires_restart,
    runtime_plan,
    secure_bearer::{HeaderCardinality, SecureBearerToken, SecureBearerTokenError, single_header},
};

const IF_CONFIG_REVISION: &str = "if-config-revision";

#[derive(Clone, Copy)]
pub(super) enum Route {
    Config,
    Validate,
}

pub(super) struct ConfigApiState {
    active_revision: ConfigRevision,
    coordinator: CanonicalConfigCoordinator,
    generations: Option<GenerationManager>,
    token: SecureBearerToken,
}

impl ConfigApiState {
    pub(super) fn new(
        coordinator: CanonicalConfigCoordinator,
        active_revision: ConfigRevision,
        token: &str,
    ) -> io::Result<Self> {
        Ok(Self {
            active_revision,
            coordinator,
            generations: None,
            token: SecureBearerToken::new(token.as_bytes()).map_err(management_token_error)?,
        })
    }

    pub(super) fn from_token_file(
        coordinator: CanonicalConfigCoordinator,
        active_revision: ConfigRevision,
        token_file: &Path,
    ) -> io::Result<Self> {
        Ok(Self {
            active_revision,
            coordinator,
            generations: None,
            token: SecureBearerToken::load(token_file).map_err(management_token_error)?,
        })
    }

    pub(super) fn set_generation_manager(&mut self, generations: GenerationManager) {
        self.generations = Some(generations);
    }

    fn active_revision(&self) -> ConfigRevision {
        self.generations
            .as_ref()
            .and_then(|generations| generations.status().active_revision)
            .unwrap_or_else(|| self.active_revision.clone())
    }

    pub(super) fn unauthenticated_response(route: Route, method: &str) -> ApiResponse {
        match (route, method) {
            (Route::Config, "GET" | "PUT") | (Route::Validate, "POST") => {
                ApiResponse::unauthorized()
            }
            (Route::Config, _) => ApiResponse::method_not_allowed("GET, PUT"),
            (Route::Validate, _) => ApiResponse::method_not_allowed("POST"),
        }
    }

    pub(super) async fn handle_http(
        &self,
        route: Route,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if !self.authorized(session) {
            return ApiResponse::unauthorized();
        }

        match (route, method) {
            (Route::Config, "GET") => self.config_response(),
            (Route::Config, "PUT") => {
                let revision = match config_revision_header(session) {
                    Ok(revision) => revision,
                    Err(response) => return response,
                };
                if let Err(response) = require_json_content_type(session) {
                    return response;
                }
                let body = match read_config_body(session).await {
                    Ok(body) => body,
                    Err(response) => return response,
                };
                match system_time_ms() {
                    Ok(now_unix_ms) => self.save_config_response(&revision, &body, now_unix_ms),
                    Err(response) => response,
                }
            }
            (Route::Config, _) => ApiResponse::method_not_allowed("GET, PUT"),
            (Route::Validate, "POST") => {
                if let Err(response) = require_json_content_type(session) {
                    return response;
                }
                let body = match read_config_body(session).await {
                    Ok(body) => body,
                    Err(response) => return response,
                };
                match system_time_ms() {
                    Ok(now_unix_ms) => self.validate_config_response(&body, now_unix_ms),
                    Err(response) => response,
                }
            }
            (Route::Validate, _) => ApiResponse::method_not_allowed("POST"),
        }
    }

    pub(super) fn authorized(&self, session: &ServerSession) -> bool {
        matches!(
            single_header(&session.req_header().headers, &AUTHORIZATION),
            HeaderCardinality::Single(value) if self.token.authorizes(value.as_bytes())
        )
    }

    fn config_response(&self) -> ApiResponse {
        match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => {
                let mut response = json!({
                    "schemaVersion": 1,
                    "diskRevision": document.disk_revision,
                    "candidateRevision": document.candidate_revision,
                    "activeRevision": self.active_revision(),
                    "config": document.normalized_config,
                    "configFormat": document.format,
                    "compositional": document.compositional,
                    "dependencyCount": document.dependencies.len(),
                    "configPreview": document.config_preview,
                    "diagnostics": document.diagnostics,
                });
                add_legacy_lua_preview(&mut response, document.format, &document.config_preview);
                ApiResponse::json(200, &response)
            }
            ConfigLoadOutcome::Rejected(rejection) => ApiResponse::json(
                503,
                &json!({
                    "schemaVersion": 1,
                    "diskRevision": rejection.disk_revision,
                    "activeRevision": self.active_revision(),
                    "diagnostics": rejection.diagnostics,
                    "error": {
                        "code": "canonical_config_unavailable",
                        "message": "the persisted canonical configuration could not be loaded",
                    },
                }),
            ),
        }
    }

    fn validate_config_response(&self, body: &[u8], now_unix_ms: u64) -> ApiResponse {
        let draft = match parse_config_request(body) {
            Ok(draft) => draft,
            Err(response) => return response,
        };
        let candidate = match self.prepare_candidate(&draft, now_unix_ms, None) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };

        let mut response = json!({
            "candidateRevision": candidate.draft.candidate_revision,
            "normalizedConfig": candidate.draft.normalized_config,
            "configFormat": candidate.draft.format,
            "compositional": candidate.draft.compositional,
            "dependencyCount": candidate.draft.dependencies.len(),
            "configPreview": candidate.draft.config_preview,
            "diagnostics": candidate.draft.diagnostics,
            "topology": candidate.topology,
            "restartRequired": candidate.restart_required,
        });
        if candidate.restart_required {
            response["diagnostics"]
                .as_array_mut()
                .expect("candidate diagnostics are an array")
                .push(restart_required_diagnostic());
        }
        add_legacy_lua_preview(
            &mut response,
            candidate.draft.format,
            &candidate.draft.config_preview,
        );
        ApiResponse::json(200, &response)
    }

    fn save_config_response(
        &self,
        expected: &ConfigRevision,
        body: &[u8],
        now_unix_ms: u64,
    ) -> ApiResponse {
        let draft = match parse_config_request(body) {
            Ok(draft) => draft,
            Err(response) => return response,
        };
        let mutation = if let Some(generations) = &self.generations {
            match generations.begin_config_mutation() {
                Ok(mutation) => Some(mutation),
                Err(error) => {
                    return ApiResponse::error(
                        409,
                        error.code(),
                        "canonical configuration mutation is unavailable",
                    );
                }
            }
        } else {
            None
        };
        let active_config = mutation
            .as_ref()
            .map(|mutation| mutation.generation().config().as_ref());
        let candidate = match self.prepare_candidate(&draft, now_unix_ms, active_config) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };
        let normalized_config = candidate.draft.normalized_config;
        let restart_required = candidate.restart_required;

        match self.coordinator.save(expected, &normalized_config) {
            ConfigSaveOutcome::Saved(document) => self.saved_response(
                &document.disk_revision,
                &document.candidate_revision,
                diagnostics_json(&document.diagnostics, false),
                restart_required,
            ),
            ConfigSaveOutcome::Conflict(conflict) => self.conflict_response(&conflict),
            ConfigSaveOutcome::InvalidDraft(rejection) => {
                ApiResponse::json(422, &json!({ "diagnostics": rejection.diagnostics }))
            }
            ConfigSaveOutcome::Failed(failure) => {
                self.failed_save_response(&failure, restart_required)
            }
        }
    }

    fn prepare_candidate(
        &self,
        draft: &Config,
        now_unix_ms: u64,
        active_config: Option<&Config>,
    ) -> Result<PreparedCandidate, ApiResponse> {
        let candidate = match self.coordinator.validate(draft) {
            ConfigValidationOutcome::Valid(candidate) => candidate,
            ConfigValidationOutcome::Invalid(rejection) => {
                return Err(ApiResponse::json(
                    422,
                    &json!({ "diagnostics": rejection.diagnostics }),
                ));
            }
        };
        let plan = prepare_config(&candidate.normalized_config).map_err(|error| {
            ApiResponse::json(
                422,
                &json!({ "diagnostics": [preparation_diagnostic(error)] }),
            )
        })?;
        let restart_required = active_config.map_or_else(
            || {
                self.generations.as_ref().is_some_and(|generations| {
                    generations.active().is_some_and(|active| {
                        unix_listener_mode_change_requires_restart(
                            active.config(),
                            &candidate.normalized_config,
                        )
                    })
                })
            },
            |active| {
                unix_listener_mode_change_requires_restart(active, &candidate.normalized_config)
            },
        );
        if let Some(generations) = &self.generations {
            let disk_revision = match self.coordinator.load() {
                ConfigLoadOutcome::Loaded(document) => document.disk_revision.clone(),
                ConfigLoadOutcome::Rejected(_) => {
                    return Err(ApiResponse::error(
                        503,
                        "canonical_config_unavailable",
                        "the persisted canonical configuration could not be loaded",
                    ));
                }
            };
            generations
                .validate_candidate(crate::config_coordinator::CanonicalConfigDocument {
                    disk_revision,
                    candidate_revision: candidate.candidate_revision.clone(),
                    normalized_config: candidate.normalized_config.clone(),
                    format: candidate.format,
                    compositional: candidate.compositional,
                    dependencies: candidate.dependencies.clone(),
                    config_preview: candidate.config_preview.clone(),
                    diagnostics: candidate.diagnostics.clone(),
                })
                .map_err(|_| {
                    ApiResponse::json(
                        422,
                        &json!({
                            "diagnostics": [preparation_diagnostic(ConfigPreparationError::Runtime)]
                        }),
                    )
                })?;
        }
        Ok(PreparedCandidate {
            topology: candidate_topology(&plan.topology, now_unix_ms),
            draft: candidate,
            restart_required,
        })
    }

    fn saved_response(
        &self,
        disk_revision: &ConfigRevision,
        candidate_revision: &ConfigRevision,
        mut diagnostics: Vec<Value>,
        restart_required: bool,
    ) -> ApiResponse {
        let active_revision = self.active_revision();
        let unchanged_active = *candidate_revision == active_revision;
        if !unchanged_active {
            diagnostics.push(if restart_required {
                restart_required_diagnostic()
            } else {
                activation_pending_diagnostic()
            });
        }
        ApiResponse::json(
            200,
            &json!({
                "diskRevision": disk_revision,
                "candidateRevision": candidate_revision,
                "activeRevision": active_revision,
                "outcome": if unchanged_active {
                    "unchanged_active"
                } else if restart_required {
                    "saved_restart_required"
                } else {
                    "saved_pending_activation"
                },
                "activationState": if unchanged_active {
                    "active"
                } else if restart_required {
                    "restart_required"
                } else {
                    "pending"
                },
                "restartRequired": !unchanged_active && restart_required,
                "diagnostics": diagnostics,
            }),
        )
    }

    fn failed_save_response(
        &self,
        failure: &ConfigSaveFailure,
        restart_required: bool,
    ) -> ApiResponse {
        let preview_disk_revision = ConfigRevision::from_bytes(failure.config_preview.as_bytes());
        if failure.disk_revision.as_ref() == Some(&preview_disk_revision) {
            return self.saved_response(
                &preview_disk_revision,
                &failure.candidate_revision,
                diagnostics_json(&failure.diagnostics, true),
                restart_required,
            );
        }
        ApiResponse::json(
            500,
            &json!({
                "diskRevision": failure.disk_revision,
                "activeRevision": self.active_revision(),
                "outcome": "write_failed",
                "diagnostics": failure.diagnostics,
            }),
        )
    }

    fn conflict_response(&self, conflict: &ConfigConflict) -> ApiResponse {
        match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => {
                let mut response = json!({
                    "schemaVersion": 1,
                    "diskRevision": document.disk_revision,
                    "candidateRevision": document.candidate_revision,
                    "activeRevision": self.active_revision(),
                    "expectedRevision": conflict.expected_revision,
                    "outcome": "conflict",
                    "config": document.normalized_config,
                    "configFormat": document.format,
                    "compositional": document.compositional,
                    "dependencyCount": document.dependencies.len(),
                    "configPreview": document.config_preview,
                    "diagnostics": conflict.diagnostics,
                });
                add_legacy_lua_preview(&mut response, document.format, &document.config_preview);
                ApiResponse::json(409, &response)
            }
            ConfigLoadOutcome::Rejected(rejection) => ApiResponse::json(
                503,
                &json!({
                    "schemaVersion": 1,
                    "diskRevision": null,
                    "activeRevision": self.active_revision(),
                    "expectedRevision": conflict.expected_revision,
                    "outcome": "authoritative_state_unavailable",
                    "diagnostics": rejection.diagnostics,
                    "error": {
                        "code": "authoritative_config_unavailable",
                        "message": "the latest persisted configuration could not be loaded",
                    },
                }),
            ),
        }
    }
}

fn add_legacy_lua_preview(response: &mut Value, format: ConfigFormat, preview: &str) {
    if format == ConfigFormat::Lua {
        response
            .as_object_mut()
            .expect("configuration response is an object")
            .insert("luaPreview".to_owned(), Value::String(preview.to_owned()));
    }
}

pub(super) fn match_route(path: &str) -> Option<Route> {
    match path {
        "/api/v1/config" => Some(Route::Config),
        "/api/v1/config/validate" => Some(Route::Validate),
        _ => None,
    }
}

pub(crate) fn preflight_management_token(path: &Path) -> io::Result<()> {
    SecureBearerToken::load(path)
        .map(drop)
        .map_err(management_token_error)
}

fn management_token_error(error: SecureBearerTokenError) -> io::Error {
    let message = match error {
        SecureBearerTokenError::Open => "management token file could not be securely opened",
        SecureBearerTokenError::NotRegular => {
            "management token file must be a regular no-follow file"
        }
        SecureBearerTokenError::InsecureMode => "management token file mode must be 0400 or 0600",
        SecureBearerTokenError::TooLarge => "management token file exceeds the supported size",
        SecureBearerTokenError::Read => "management token file could not be read",
        SecureBearerTokenError::Unstable => "management token file changed while it was read",
        SecureBearerTokenError::InvalidToken => {
            "management token must be 32 to 512 visible ASCII bytes"
        }
    };
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
enum ConfigPreparationError {
    #[error("runtime configuration preflight failed")]
    Runtime,
    #[error("configured management UI assets are not readable")]
    UiAssets,
    #[error("Certbot watcher prerequisites are unavailable")]
    CertbotWatcher,
    #[error("direct-file watcher prerequisites are unavailable")]
    DirectFileWatcher,
}

struct PreparedCandidate {
    draft: Box<ValidatedConfigDraft>,
    restart_required: bool,
    topology: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    config: Config,
}

fn parse_config_request(body: &[u8]) -> Result<Config, ApiResponse> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        ApiResponse::error(
            400,
            "malformed_json",
            "request body is not well-formed JSON",
        )
    })?;
    serde_json::from_value::<ConfigRequest>(value)
        .map(|request| request.config)
        .map_err(|_| {
            ApiResponse::json(
                422,
                &json!({
                    "diagnostics": [{
                        "code": "E_INVALID_FIELD",
                        "severity": "error",
                        "stage": "parse",
                        "path": "/config",
                        "message": "the config field does not match the canonical configuration schema",
                    }],
                }),
            )
        })
}

fn prepare_config(config: &Config) -> Result<RuntimePlan, ConfigPreparationError> {
    let plan = runtime_plan(config).map_err(|_| ConfigPreparationError::Runtime)?;
    if let Some(ui_dir) = config
        .management
        .as_ref()
        .and_then(|management| management.ui_dir.as_deref())
    {
        UiAssets::load(ui_dir).map_err(|_| ConfigPreparationError::UiAssets)?;
    }
    plan.tls
        .check_certbot_watcher(CertbotWatcherConfig::default())
        .map_err(|_| ConfigPreparationError::CertbotWatcher)?;
    plan.tls
        .check_file_watcher(FileWatcherConfig::default())
        .map_err(|_| ConfigPreparationError::DirectFileWatcher)?;
    Ok(plan)
}

fn preparation_diagnostic(error: ConfigPreparationError) -> Value {
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

fn activation_pending_diagnostic() -> Value {
    json!({
        "code": "I_ACTIVATION_PENDING",
        "severity": "warning",
        "stage": "activation",
        "message": "configuration was saved and queued for generation activation",
    })
}

fn restart_required_diagnostic() -> Value {
    json!({
        "code": "I_RESTART_REQUIRED",
        "severity": "warning",
        "stage": "activation",
        "path": "/config/listeners",
        "message": "an active Unix listener mode changed; the saved configuration takes effect after a process restart",
    })
}

fn diagnostics_json(diagnostics: &[ConfigDiagnostic], as_warnings: bool) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code,
                "severity": if as_warnings { "warning" } else {
                    match diagnostic.severity {
                        crate::config_coordinator::ConfigDiagnosticSeverity::Error => "error",
                        crate::config_coordinator::ConfigDiagnosticSeverity::Warning => "warning",
                    }
                },
                "stage": diagnostic.stage,
                "message": diagnostic.message,
            })
        })
        .collect()
}

fn config_revision_header(session: &ServerSession) -> Result<ConfigRevision, ApiResponse> {
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
        .and_then(|value| ConfigRevision::from_str(value).ok())
        .ok_or_else(|| {
            ApiResponse::error(
                400,
                "invalid_config_revision",
                "If-Config-Revision must contain one raw 64-hex configuration revision",
            )
        })
}

fn require_json_content_type(session: &ServerSession) -> Result<(), ApiResponse> {
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
