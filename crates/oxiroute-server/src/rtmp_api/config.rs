use std::{io, path::Path, str::FromStr};

use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use oxiroute_config::{Config, RtmpAccessPolicy};
use oxiroute_config_source::{ConfigFormat, render_config};
use oxiroute_import::ImportReportEnvelope;
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
        NativeImportSourceOutcome, ValidatedConfigDraft,
    },
    listener_reservation::unix_listener_mode_change_requires_restart,
    runtime_plan,
    secure_bearer::{HeaderCardinality, SecureBearerToken, SecureBearerTokenError, single_header},
};

const IF_CONFIG_REVISION: &str = "if-config-revision";
const IMPORT_REPORTS_PATH: &str = "/api/v1/import-reports";
const MAX_IMPORT_REPORTS: usize = 64;
const MAX_IMPORT_REPORT_RESPONSE_BYTES: usize = MAX_CONFIG_REQUEST_BYTES * 2;
const REDACTED_IMPORT_VALUE: &str = "<redacted>";

#[derive(Clone, Copy)]
pub(super) enum Route {
    Config,
    Validate,
    ImportReports(Option<usize>),
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
            (Route::ImportReports(_), "GET") => ApiResponse::unauthorized(),
            (Route::Config, _) => ApiResponse::method_not_allowed("GET, PUT"),
            (Route::Validate, _) => ApiResponse::method_not_allowed("POST"),
            (Route::ImportReports(_), _) => ApiResponse::method_not_allowed("GET"),
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
            (Route::ImportReports(index), "GET") => self.import_reports_response(index),
            (Route::ImportReports(_), _) => ApiResponse::method_not_allowed("GET"),
        }
    }

    pub(super) fn authorized(&self, session: &ServerSession) -> bool {
        matches!(
            single_header(&session.req_header().headers, &AUTHORIZATION),
            HeaderCardinality::Single(value) if self.token.authorizes(value.as_bytes())
        )
    }

    fn merge_redacted_rtmp_token_secrets(&self, draft: &mut Config) -> Result<(), ApiResponse> {
        if !contains_redacted_rtmp_token_secret(draft) {
            return Ok(());
        }
        let authoritative = match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document.normalized_config,
            ConfigLoadOutcome::Rejected(rejection) => {
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
        Ok(())
    }

    fn config_response(&self) -> ApiResponse {
        match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => {
                let (config, config_preview) =
                    redacted_config_view(document.normalized_config, document.format);
                let mut response = json!({
                    "schemaVersion": 1,
                    "diskRevision": document.disk_revision,
                    "candidateRevision": document.candidate_revision,
                    "activeRevision": self.active_revision(),
                    "config": config,
                    "configFormat": document.format,
                    "compositional": document.compositional,
                    "dependencyCount": document.dependencies.len(),
                    "configPreview": config_preview,
                    "diagnostics": document.diagnostics,
                });
                add_legacy_lua_preview(&mut response, document.format, &config_preview);
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
                let report = redacted_import_report(reference.evidence.clone());
                import_report_summary(index, &report)
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
                Some(redacted_import_report(reference.evidence.clone()))
            }
            None => None,
        };
        let preview = selected.as_ref().and_then(import_report_preview);
        let response = json!({
            "schemaVersion": 1,
            "diskRevision": source.disk_revision,
            "candidateRevision": source.candidate_revision,
            "activeRevision": self.active_revision(),
            "configFormat": source.format,
            "compositional": source.compositional,
            "reports": reports,
            "selection": selection.map(|index| json!({ "index": index })),
            "report": selected,
            "preview": preview,
            "diagnostics": [],
        });
        bounded_import_report_response(response)
    }

    fn validate_config_response(&self, body: &[u8], now_unix_ms: u64) -> ApiResponse {
        let mut draft = match parse_config_request(body) {
            Ok(draft) => draft,
            Err(response) => return response,
        };
        if let Err(response) = self.merge_redacted_rtmp_token_secrets(&mut draft) {
            return response;
        }
        let candidate = match self.prepare_candidate(&draft, now_unix_ms, None) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };
        let (normalized_config, config_preview) = redacted_config_view(
            candidate.draft.normalized_config.clone(),
            candidate.draft.format,
        );

        let mut response = json!({
            "candidateRevision": candidate.draft.candidate_revision,
            "normalizedConfig": normalized_config,
            "configFormat": candidate.draft.format,
            "compositional": candidate.draft.compositional,
            "dependencyCount": candidate.draft.dependencies.len(),
            "configPreview": config_preview,
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
        add_legacy_lua_preview(&mut response, candidate.draft.format, &config_preview);
        ApiResponse::json(200, &response)
    }

    fn save_config_response(
        &self,
        expected: &ConfigRevision,
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
                let (config, config_preview) =
                    redacted_config_view(document.normalized_config, document.format);
                let mut response = json!({
                    "schemaVersion": 1,
                    "diskRevision": document.disk_revision,
                    "candidateRevision": document.candidate_revision,
                    "activeRevision": self.active_revision(),
                    "expectedRevision": conflict.expected_revision,
                    "outcome": "conflict",
                    "config": config,
                    "configFormat": document.format,
                    "compositional": document.compositional,
                    "dependencyCount": document.dependencies.len(),
                    "configPreview": config_preview,
                    "diagnostics": conflict.diagnostics,
                });
                add_legacy_lua_preview(&mut response, document.format, &config_preview);
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

const REDACTED_RTMP_TOKEN: &str = "<redacted>";

fn redacted_config_view(mut config: Config, format: ConfigFormat) -> (Config, String) {
    for service in &mut config.rtmp_services {
        for application in &mut service.applications {
            for policy in [&mut application.publish, &mut application.play] {
                if let Some(token) = policy.token.as_mut() {
                    token.secret = REDACTED_RTMP_TOKEN.into();
                }
            }
        }
    }
    let preview = render_config(format, &config)
        .expect("normalized configuration remains renderable after token redaction");
    (config, preview)
}

fn contains_redacted_rtmp_token_secret(config: &Config) -> bool {
    config.rtmp_services.iter().any(|service| {
        service.applications.iter().any(|application| {
            [&application.publish, &application.play]
                .into_iter()
                .any(|policy| {
                    policy
                        .token
                        .as_ref()
                        .is_some_and(|token| token.secret == REDACTED_RTMP_TOKEN)
                })
        })
    })
}

fn restore_redacted_rtmp_token_secrets(draft: &mut Config, authoritative: &Config) {
    for service in &mut draft.rtmp_services {
        let Some(authoritative_service) = authoritative
            .rtmp_services
            .iter()
            .find(|candidate| candidate.name == service.name)
        else {
            continue;
        };
        for application in &mut service.applications {
            let Some(authoritative_application) = authoritative_service
                .applications
                .iter()
                .find(|candidate| candidate.name == application.name)
            else {
                continue;
            };
            restore_redacted_rtmp_token_secret(
                &mut application.publish,
                &authoritative_application.publish,
            );
            restore_redacted_rtmp_token_secret(
                &mut application.play,
                &authoritative_application.play,
            );
        }
    }
}

fn restore_redacted_rtmp_token_secret(
    draft: &mut RtmpAccessPolicy,
    authoritative: &RtmpAccessPolicy,
) {
    let Some(token) = draft.token.as_mut() else {
        return;
    };
    if token.secret != REDACTED_RTMP_TOKEN {
        return;
    }
    let Some(authoritative_token) = authoritative.token.as_ref() else {
        return;
    };
    token.secret.clone_from(&authoritative_token.secret);
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

fn import_report_summary(index: usize, report: &ImportReportEnvelope) -> Value {
    let status = if !report.blockers.is_empty() {
        "blocked"
    } else if report.candidate.finalized {
        "finalized"
    } else {
        "draft"
    };
    json!({
        "index": index,
        "product": report.source.product,
        "version": report.source.version,
        "versionSource": report.source.version_source,
        "capabilityProfile": report.source.capability_profile,
        "status": status,
        "rootCount": report.source_graph.roots.len(),
        "sourceCount": report.source_graph.sources.len(),
        "dependencyCount": report.source_graph.dependencies.len(),
        "blockerCount": report.blockers.len(),
        "diagnosticCount": report.diagnostics.len(),
        "provenanceCount": report.candidate.provenance.len(),
        "requirementCount": report.requirements.deployment.len() + report.requirements.activation.len(),
        "overlayCount": report.overlays.len(),
        "previewAvailable": import_report_preview(report).is_some(),
    })
}

fn import_report_preview(report: &ImportReportEnvelope) -> Option<Value> {
    if !report.candidate.finalized || !report.blockers.is_empty() {
        return None;
    }
    let config = report.candidate.config.as_ref()?;
    let (_, text) = redacted_config_view(config.clone(), ConfigFormat::Kdl);
    Some(json!({ "format": "kdl", "text": text }))
}

fn bounded_import_report_response(value: Value) -> ApiResponse {
    let body = value.to_string().into_bytes();
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

fn redacted_import_report(mut report: ImportReportEnvelope) -> ImportReportEnvelope {
    for root in &mut report.source_graph.roots {
        root.path = root.path.as_ref().map(|_| REDACTED_IMPORT_VALUE.to_owned());
    }
    for source in &mut report.source_graph.sources {
        source.name = format!("source-{}", source.id);
        source.path = source
            .path
            .as_ref()
            .map(|_| REDACTED_IMPORT_VALUE.to_owned());
    }
    for dependency in &mut report.source_graph.dependencies {
        dependency.requested_path = dependency
            .requested_path
            .as_ref()
            .map(|_| REDACTED_IMPORT_VALUE.to_owned());
        dependency.canonical_path = dependency
            .canonical_path
            .as_ref()
            .map(|_| REDACTED_IMPORT_VALUE.to_owned());
    }
    for provenance in &mut report.candidate.provenance {
        for origin in &mut provenance.origins {
            redact_origin(origin);
        }
    }
    for requirement in report
        .requirements
        .deployment
        .iter_mut()
        .chain(report.requirements.activation.iter_mut())
    {
        requirement.values.fill(REDACTED_IMPORT_VALUE.to_owned());
        for origin in &mut requirement.origins {
            redact_origin(origin);
        }
    }
    for overlay in &mut report.overlays {
        overlay.values.fill(REDACTED_IMPORT_VALUE.to_owned());
        if let Some(origin) = &mut overlay.origin {
            redact_origin(origin);
        }
    }
    for blocker in &mut report.blockers {
        blocker.scope = blocker.scope.as_deref().map(redact_scope);
    }
    for diagnostic in &mut report.diagnostics {
        diagnostic.help = diagnostic
            .help
            .as_ref()
            .map(|_| REDACTED_IMPORT_VALUE.to_owned());
    }
    if let Some(config) = report.candidate.config.take() {
        let (config, _) = redacted_config_view(config, ConfigFormat::Kdl);
        report.candidate.config = Some(config);
    }
    report
}

fn redact_origin(origin: &mut oxiroute_import::OriginEvidence) {
    origin.path = origin
        .path
        .as_ref()
        .map(|_| REDACTED_IMPORT_VALUE.to_owned());
}

fn redact_scope(scope: &str) -> String {
    if scope.contains('/') || scope.contains('\\') {
        REDACTED_IMPORT_VALUE.to_owned()
    } else {
        scope.to_owned()
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
