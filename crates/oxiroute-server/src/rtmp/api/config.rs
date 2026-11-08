use std::{str::FromStr, sync::Arc};

use http::header::{CONTENT_LENGTH, CONTENT_TYPE, IF_NONE_MATCH};
use oxiroute_config::{ConfigDraft, ValidatedConfig};
use pingora::protocols::http::ServerSession;
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    ApiResponse, MAX_CONFIG_REQUEST_BYTES,
    auth::ManagementAuth,
    dto::{
        ConfigConflictResponse, ConfigRequest, ConfigValidationResponse, ImportReportResponse,
        RedactedConfigView, RedactedImportReport, contains_redacted_rtmp_token_secret,
        restore_redacted_rtmp_token_secrets,
    },
    observability::candidate_topology,
    response::system_time_ms,
    ui::UiAssets,
};
use crate::{
    CertbotWatcherConfig, FileWatcherConfig, GenerationManager, RuntimeMode, RuntimePlan,
    config::coordinator::{
        AuthoredRevision, CanonicalConfigCoordinator, ConfigConflict, ConfigDiagnostic,
        ConfigLoadOutcome, ConfigSaveFailure, ConfigSaveOutcome, ConfigValidationOutcome,
        EffectiveRevision, NativeImportSourceOutcome, PersistableConfigCandidate,
    },
    listener::inventory::{ListenerChangeSummary, ListenerRestartReason},
    service_plan::validation_plan,
};

mod snapshot;

use snapshot::SnapshotCache;

const IF_CONFIG_REVISION: &str = "if-config-revision";
const IMPORT_REPORTS_PATH: &str = "/api/v1/import-reports";
const MAX_IMPORT_REPORTS: usize = 64;
const MAX_IMPORT_REPORT_RESPONSE_BYTES: usize = MAX_CONFIG_REQUEST_BYTES * 2;

#[derive(Clone, Copy)]
pub(super) enum Route {
    Config,
    Validate,
    ImportReports(Option<usize>),
}

pub(super) struct ConfigApiState {
    active_revision: EffectiveRevision,
    coordinator: CanonicalConfigCoordinator,
    generations: Option<GenerationManager>,
    mode: RuntimeMode,
    snapshot: Arc<SnapshotCache>,
}

impl ConfigApiState {
    pub(super) fn new(
        coordinator: CanonicalConfigCoordinator,
        active_revision: EffectiveRevision,
        mode: RuntimeMode,
    ) -> Self {
        Self {
            active_revision,
            coordinator,
            generations: None,
            mode,
            snapshot: Arc::default(),
        }
    }

    pub(super) fn set_generation_manager(&mut self, generations: GenerationManager) {
        self.generations = Some(generations);
    }

    fn active_revision(&self) -> EffectiveRevision {
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
            (Route::ImportReports(_), "GET") => ApiResponse::unauthorized(),
            (Route::Config, _) => ApiResponse::method_not_allowed("GET, PUT"),
            (Route::Validate, _) => ApiResponse::method_not_allowed("POST"),
            (Route::ImportReports(_), _) => ApiResponse::method_not_allowed("GET"),
        }
    }

    pub(super) async fn handle_http(
        &self,
        auth: &ManagementAuth,
        route: Route,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if !auth.authorized(session) {
            return ApiResponse::unauthorized();
        }

        match (route, method) {
            (Route::Config, "GET") => {
                let if_none_match = session
                    .req_header()
                    .headers
                    .get(IF_NONE_MATCH)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                self.config_response(if_none_match).await
            }
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
            (Route::ImportReports(index), "GET") => self.import_reports_response(index),
            (Route::ImportReports(_), _) => ApiResponse::method_not_allowed("GET"),
        }
    }

    fn merge_redacted_rtmp_token_secrets(
        &self,
        draft: &mut ConfigDraft,
    ) -> Result<(), ApiResponse> {
        if !contains_redacted_rtmp_token_secret(draft) {
            return Ok(());
        }
        let authoritative = match self.coordinator.load_shared() {
            Ok(document) => document.validated_config.to_draft(),
            Err(rejection) => {
                return Err(ApiResponse::json(
                    503,
                    &json!({
                        "schemaVersion": 1,
                        "diskRevision": rejection.disk_revision,
                        "activeRevision": self.active_revision(),
                        "diagnostics": rejection.diagnostics,
                        "error": {
                            "code": "authoritative_config_unavailable",
                            "message": "the latest persisted configuration could not be loaded",
                        },
                    }),
                ));
            }
        };
        restore_redacted_rtmp_token_secrets(draft, &authoritative);
        if contains_redacted_rtmp_token_secret(draft) {
            return Err(ApiResponse::error(
                422,
                "redacted_rtmp_token_unresolved",
                "redacted RTMP token secrets must match an existing authoritative token",
            ));
        }
        Ok(())
    }

    async fn config_response(&self, if_none_match: Option<String>) -> ApiResponse {
        let coordinator = self.coordinator.clone();
        let snapshot = Arc::clone(&self.snapshot);
        let active = self.active_revision();
        tokio::task::spawn_blocking(move || {
            snapshot.response(&coordinator, &active, if_none_match.as_deref())
        })
        .await
        .unwrap_or_else(|_| {
            ApiResponse::error(
                503,
                "config_read_failed",
                "the configuration read worker failed",
            )
        })
    }

    fn import_reports_response(&self, selection: Option<usize>) -> ApiResponse {
        let source = match self.coordinator.load_native_import_source() {
            NativeImportSourceOutcome::Loaded(source) => source,
            NativeImportSourceOutcome::Rejected(rejection) => {
                return ApiResponse::json(
                    503,
                    &json!({
                        "schemaVersion": 1,
                        "diskRevision": rejection.disk_revision,
                        "activeRevision": self.active_revision(),
                        "reports": [],
                        "selection": null,
                        "report": null,
                        "preview": null,
                        "diagnostics": rejection.diagnostics,
                        "error": {
                            "code": "canonical_config_unavailable",
                            "message": "the persisted canonical configuration could not be loaded",
                        },
                    }),
                );
            }
        };
        if source.native_references.len() > MAX_IMPORT_REPORTS {
            return ApiResponse::error(
                413,
                "import_report_limit_exceeded",
                format!("native import reports are limited to {MAX_IMPORT_REPORTS} entries"),
            );
        }

        let reports = source
            .native_references
            .iter()
            .enumerate()
            .map(|(index, reference)| {
                RedactedImportReport::new(reference.evidence.clone()).summary(index)
            })
            .collect::<Vec<_>>();
        let selected = match selection {
            Some(index) => {
                let Some(reference) = source.native_references.get(index) else {
                    return ApiResponse::error(
                        404,
                        "import_report_not_found",
                        "the requested native import report does not exist",
                    );
                };
                Some(RedactedImportReport::new(reference.evidence.clone()))
            }
            None => None,
        };
        let response = ImportReportResponse::new(
            &source,
            self.active_revision(),
            reports,
            selection,
            selected,
        );
        bounded_import_report_response(&response)
    }

    fn validate_config_response(&self, body: &[u8], now_unix_ms: u64) -> ApiResponse {
        let mut draft = match parse_config_request(body) {
            Ok(draft) => draft,
            Err(response) => return response,
        };
        if let Err(response) = self.merge_redacted_rtmp_token_secrets(&mut draft) {
            return response;
        }
        let candidate = match self.prepare_candidate(draft, now_unix_ms, None) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };
        let view = RedactedConfigView::of(
            candidate.draft.validated_config(),
            candidate.draft.format(),
            candidate.draft.config_preview(),
        );
        let mut diagnostics = diagnostics_json(candidate.draft.diagnostics(), false);
        if let Some(reason) = candidate.restart_reason {
            diagnostics.push(restart_required_diagnostic(reason));
        } else if let Some(changes) = &candidate.listener_changes {
            diagnostics.push(listener_changes_diagnostic(self.mode, changes));
        }
        ApiResponse::dto(
            200,
            &ConfigValidationResponse::new(
                candidate.draft.effective_revision().clone(),
                candidate.draft.format(),
                candidate.draft.compositional(),
                candidate.draft.dependencies().len(),
                view,
                diagnostics,
                candidate.topology,
                candidate.restart_reason.is_some(),
            ),
        )
    }

    fn save_config_response(
        &self,
        expected: &AuthoredRevision,
        body: &[u8],
        now_unix_ms: u64,
    ) -> ApiResponse {
        let mut draft = match parse_config_request(body) {
            Ok(draft) => draft,
            Err(response) => return response,
        };
        if let Err(response) = self.merge_redacted_rtmp_token_secrets(&mut draft) {
            return response;
        }
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
        let candidate = match self.prepare_candidate(draft, now_unix_ms, mutation.as_ref()) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };
        let PreparedCandidate {
            draft: candidate,
            restart_reason,
            listener_changes,
            ..
        } = candidate;
        let activation = Activation {
            restart_reason,
            listener_changes,
        };

        match self.coordinator.save(expected, *candidate) {
            ConfigSaveOutcome::Saved(document) => self.saved_response(
                &document.authored_revision,
                &document.effective_revision,
                diagnostics_json(&document.diagnostics, false),
                &activation,
            ),
            ConfigSaveOutcome::Conflict(conflict) => self.conflict_response(&conflict),
            ConfigSaveOutcome::InvalidDraft(rejection) => {
                ApiResponse::json(422, &json!({ "diagnostics": rejection.diagnostics }))
            }
            ConfigSaveOutcome::Failed(failure) => self.failed_save_response(&failure, &activation),
        }
    }

    fn prepare_candidate(
        &self,
        draft: ConfigDraft,
        now_unix_ms: u64,
        mutation: Option<&crate::GenerationMutation>,
    ) -> Result<PreparedCandidate, ApiResponse> {
        let candidate = match self.coordinator.prepare(draft) {
            ConfigValidationOutcome::Valid(candidate) => candidate,
            ConfigValidationOutcome::Invalid(rejection) => {
                return Err(ApiResponse::json(
                    422,
                    &json!({ "diagnostics": rejection.diagnostics }),
                ));
            }
        };
        let plan = prepare_config(candidate.validated_config()).map_err(|error| {
            ApiResponse::json(
                422,
                &json!({ "diagnostics": [preparation_diagnostic(error)] }),
            )
        })?;
        let inspect = |generation: &crate::RuntimeGeneration| {
            (
                generation.listener_restart_reason(candidate.validated_config()),
                generation
                    .listener_changes(candidate.validated_config())
                    .filter(ListenerChangeSummary::changes_listeners),
            )
        };
        let (restart_reason, listener_changes) = mutation.map_or_else(
            || {
                self.generations
                    .as_ref()
                    .and_then(GenerationManager::active)
                    .map_or((None, None), |active| inspect(&active))
            },
            |mutation| inspect(mutation.generation()),
        );
        if let Some(generations) = &self.generations {
            let disk_revision = match self.coordinator.load_shared() {
                Ok(document) => document.authored_revision.clone(),
                Err(_) => {
                    return Err(ApiResponse::error(
                        503,
                        "canonical_config_unavailable",
                        "the persisted canonical configuration could not be loaded",
                    ));
                }
            };
            generations
                .validate_candidate(crate::config::coordinator::ResolvedConfigDocument {
                    authored_revision: disk_revision,
                    effective_revision: candidate.effective_revision().clone(),
                    validated_config: candidate.validated_config().clone(),
                    format: candidate.format(),
                    compositional: candidate.compositional(),
                    dependencies: candidate.dependencies().to_vec(),
                    config_preview: candidate.config_preview().to_owned(),
                    diagnostics: candidate.diagnostics().to_vec(),
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
            restart_reason,
            listener_changes,
        })
    }

    fn saved_response(
        &self,
        disk_revision: &AuthoredRevision,
        candidate_revision: &EffectiveRevision,
        mut diagnostics: Vec<Value>,
        activation: &Activation,
    ) -> ApiResponse {
        let active_revision = self.active_revision();
        let unchanged_active = *candidate_revision == active_revision;
        let restart_reason = activation.restart_reason;
        if !unchanged_active {
            // The activation state stays the last diagnostic, as clients read it.
            if restart_reason.is_none()
                && let Some(changes) = &activation.listener_changes
            {
                diagnostics.push(listener_changes_diagnostic(self.mode, changes));
            }
            diagnostics.push(if let Some(reason) = restart_reason {
                restart_required_diagnostic(reason)
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
                } else if restart_reason.is_some() {
                    "saved_restart_required"
                } else {
                    "saved_pending_activation"
                },
                "activationState": if unchanged_active {
                    "active"
                } else if restart_reason.is_some() {
                    "restart_required"
                } else {
                    "pending"
                },
                "restartRequired": !unchanged_active && restart_reason.is_some(),
                "diagnostics": diagnostics,
            }),
        )
    }

    fn failed_save_response(
        &self,
        failure: &ConfigSaveFailure,
        activation: &Activation,
    ) -> ApiResponse {
        let preview_disk_revision = AuthoredRevision::from_bytes(failure.config_preview.as_bytes());
        if failure.disk_revision.as_ref() == Some(&preview_disk_revision) {
            return self.saved_response(
                &preview_disk_revision,
                &failure.candidate_revision,
                diagnostics_json(&failure.diagnostics, true),
                activation,
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
                let view = RedactedConfigView::new(&document.validated_config, document.format);
                ApiResponse::dto(
                    409,
                    &ConfigConflictResponse::new(
                        *document,
                        self.active_revision(),
                        conflict.expected_revision.clone(),
                        conflict.diagnostics.clone(),
                        view,
                    ),
                )
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

pub(super) fn match_route(path: &str) -> Option<Route> {
    match path {
        "/api/v1/config" => Some(Route::Config),
        "/api/v1/config/validate" => Some(Route::Validate),
        IMPORT_REPORTS_PATH => Some(Route::ImportReports(None)),
        path if path.starts_with("/api/v1/import-reports/") => Some(Route::ImportReports(
            path.strip_prefix("/api/v1/import-reports/")
                .filter(|value| !value.is_empty() && !value.contains('/'))
                .and_then(|value| value.parse().ok())
                .or(Some(usize::MAX)),
        )),
        _ => None,
    }
}

fn bounded_import_report_response(value: &impl Serialize) -> ApiResponse {
    let value = serde_json::to_value(value).expect("import report response DTO serializes");
    let body = serde_json::to_vec(&value).expect("import report response JSON serializes");
    if body.len() > MAX_IMPORT_REPORT_RESPONSE_BYTES {
        return ApiResponse::error(
            413,
            "import_report_response_too_large",
            format!(
                "native import report response exceeds {MAX_IMPORT_REPORT_RESPONSE_BYTES} bytes"
            ),
        );
    }
    ApiResponse::bytes(200, body, "application/json")
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
    draft: Box<PersistableConfigCandidate>,
    restart_reason: Option<ListenerRestartReason>,
    listener_changes: Option<ListenerChangeSummary>,
    topology: Value,
}

/// How a saved configuration reaches the running process.
struct Activation {
    restart_reason: Option<ListenerRestartReason>,
    listener_changes: Option<ListenerChangeSummary>,
}

fn parse_config_request(body: &[u8]) -> Result<ConfigDraft, ApiResponse> {
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

fn prepare_config(config: &ValidatedConfig) -> Result<RuntimePlan, ConfigPreparationError> {
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

fn restart_required_diagnostic(reason: ListenerRestartReason) -> Value {
    let message = match reason {
        ListenerRestartReason::UnixModeChange => {
            "an active Unix listener mode changed; the saved configuration takes effect after a process restart"
        }
    };
    json!({
        "code": "I_RESTART_REQUIRED",
        "severity": "warning",
        "stage": "activation",
        "path": "/config/listeners",
        "message": message,
    })
}

fn listener_changes_diagnostic(mode: RuntimeMode, changes: &ListenerChangeSummary) -> Value {
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

fn diagnostics_json(diagnostics: &[ConfigDiagnostic], as_warnings: bool) -> Vec<Value> {
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

fn config_revision_header(session: &ServerSession) -> Result<AuthoredRevision, ApiResponse> {
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
