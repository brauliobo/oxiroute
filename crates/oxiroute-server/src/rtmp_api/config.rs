use std::{
    fs::File,
    io::{self, Read as _},
    path::Path,
    str::FromStr,
};

use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use openssl::{memcmp, sha::sha256};
use oxiroute_config::Config;
use oxiroute_config_source::ConfigFormat;
use pingora::protocols::http::ServerSession;
use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{
    ApiResponse, MAX_CONFIG_REQUEST_BYTES, observability::candidate_topology,
    response::system_time_ms, ui::UiAssets,
};
use crate::{
    CertbotWatcherConfig, GenerationManager, RuntimePlan,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigConflict, ConfigDiagnostic, ConfigLoadOutcome,
        ConfigRevision, ConfigSaveFailure, ConfigSaveOutcome, ConfigValidationOutcome,
        ValidatedConfigDraft,
    },
    runtime_plan,
};

const MIN_MANAGEMENT_TOKEN_BYTES: usize = 32;
const MAX_MANAGEMENT_TOKEN_BYTES: usize = 512;
const MAX_MANAGEMENT_TOKEN_FILE_BYTES: usize = MAX_MANAGEMENT_TOKEN_BYTES + 2;
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
    token: ManagementToken,
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
            token: ManagementToken::new(token.as_bytes()).map_err(|error| {
                io::Error::new(io::ErrorKind::PermissionDenied, error.to_string())
            })?,
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
            token: ManagementToken::load(token_file).map_err(|error| {
                io::Error::new(io::ErrorKind::PermissionDenied, error.to_string())
            })?,
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
        let mut values = session.req_header().headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        values.next().is_none() && self.token.authorizes(value.as_bytes())
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
        let candidate = match self.prepare_candidate(&draft, now_unix_ms) {
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
        });
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
        let candidate = match self.prepare_candidate(&draft, now_unix_ms) {
            Ok(candidate) => candidate,
            Err(response) => return response,
        };
        let _mutation = if let Some(generations) = &self.generations {
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
        let normalized_config = candidate.draft.normalized_config;

        match self.coordinator.save(expected, &normalized_config) {
            ConfigSaveOutcome::Saved(document) => self.saved_response(
                &document.disk_revision,
                &document.candidate_revision,
                diagnostics_json(&document.diagnostics, false),
            ),
            ConfigSaveOutcome::Conflict(conflict) => self.conflict_response(&conflict),
            ConfigSaveOutcome::InvalidDraft(rejection) => {
                ApiResponse::json(422, &json!({ "diagnostics": rejection.diagnostics }))
            }
            ConfigSaveOutcome::Failed(failure) => self.failed_save_response(&failure),
        }
    }

    fn prepare_candidate(
        &self,
        draft: &Config,
        now_unix_ms: u64,
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
        })
    }

    fn saved_response(
        &self,
        disk_revision: &ConfigRevision,
        candidate_revision: &ConfigRevision,
        mut diagnostics: Vec<Value>,
    ) -> ApiResponse {
        let active_revision = self.active_revision();
        let unchanged_active = *candidate_revision == active_revision;
        if !unchanged_active {
            diagnostics.push(activation_pending_diagnostic());
        }
        ApiResponse::json(
            200,
            &json!({
                "diskRevision": disk_revision,
                "candidateRevision": candidate_revision,
                "activeRevision": active_revision,
                "outcome": if unchanged_active {
                    "unchanged_active"
                } else {
                    "saved_pending_activation"
                },
                "activationState": if unchanged_active { "active" } else { "pending" },
                "restartRequired": false,
                "diagnostics": diagnostics,
            }),
        )
    }

    fn failed_save_response(&self, failure: &ConfigSaveFailure) -> ApiResponse {
        let preview_disk_revision = ConfigRevision::from_bytes(failure.config_preview.as_bytes());
        if failure.disk_revision.as_ref() == Some(&preview_disk_revision) {
            return self.saved_response(
                &preview_disk_revision,
                &failure.candidate_revision,
                diagnostics_json(&failure.diagnostics, true),
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

struct ManagementToken {
    digest: [u8; 32],
}

impl ManagementToken {
    fn new(token: &[u8]) -> Result<Self, ManagementTokenError> {
        validate_management_token(token)?;
        Ok(Self {
            digest: sha256(token),
        })
    }

    fn load(path: &Path) -> Result<Self, ManagementTokenError> {
        let descriptor = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| ManagementTokenError::Open)?;
        let before = rustix_fs::fstat(&descriptor).map_err(|_| ManagementTokenError::Read)?;
        if !FileType::from_raw_mode(before.st_mode).is_file() {
            return Err(ManagementTokenError::NotRegular);
        }
        if !matches!(before.st_mode & 0o7777, 0o400 | 0o600) {
            return Err(ManagementTokenError::InsecureMode);
        }
        let size = usize::try_from(before.st_size).map_err(|_| ManagementTokenError::TooLarge)?;
        if size > MAX_MANAGEMENT_TOKEN_FILE_BYTES {
            return Err(ManagementTokenError::TooLarge);
        }

        let mut file = File::from(descriptor);
        let mut bytes = Zeroizing::new(Vec::with_capacity(size));
        file.by_ref()
            .take(
                u64::try_from(MAX_MANAGEMENT_TOKEN_FILE_BYTES + 1)
                    .expect("management token limit fits u64"),
            )
            .read_to_end(&mut bytes)
            .map_err(|_| ManagementTokenError::Read)?;
        if bytes.len() > MAX_MANAGEMENT_TOKEN_FILE_BYTES {
            return Err(ManagementTokenError::TooLarge);
        }
        let after = rustix_fs::fstat(&file).map_err(|_| ManagementTokenError::Read)?;
        if before.st_dev != after.st_dev
            || before.st_ino != after.st_ino
            || before.st_size != after.st_size
            || before.st_mode != after.st_mode
        {
            return Err(ManagementTokenError::Unstable);
        }
        trim_one_line_ending(&mut bytes);
        Self::new(&bytes)
    }

    fn authorizes(&self, authorization: &[u8]) -> bool {
        let Some(candidate) = authorization.strip_prefix(b"Bearer ") else {
            return false;
        };
        memcmp::eq(&self.digest, &sha256(candidate))
    }
}

pub(crate) fn preflight_management_token(path: &Path) -> io::Result<()> {
    ManagementToken::load(path)
        .map(drop)
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))
}

#[derive(Debug, thiserror::Error)]
enum ManagementTokenError {
    #[error("management token file could not be securely opened")]
    Open,
    #[error("management token file must be a regular no-follow file")]
    NotRegular,
    #[error("management token file mode must be 0400 or 0600")]
    InsecureMode,
    #[error("management token file exceeds the supported size")]
    TooLarge,
    #[error("management token file could not be read")]
    Read,
    #[error("management token file changed while it was read")]
    Unstable,
    #[error("management token must be 32 to 512 visible ASCII bytes")]
    InvalidToken,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
enum ConfigPreparationError {
    #[error("runtime configuration preflight failed")]
    Runtime,
    #[error("configured management UI assets are not readable")]
    UiAssets,
    #[error("Certbot watcher prerequisites are unavailable")]
    CertbotWatcher,
}

struct PreparedCandidate {
    draft: Box<ValidatedConfigDraft>,
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

fn trim_one_line_ending(bytes: &mut Vec<u8>) {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
}

fn validate_management_token(token: &[u8]) -> Result<(), ManagementTokenError> {
    if !(MIN_MANAGEMENT_TOKEN_BYTES..=MAX_MANAGEMENT_TOKEN_BYTES).contains(&token.len())
        || std::str::from_utf8(token).is_err()
        || !token.iter().all(|byte| matches!(byte, 0x21..=0x7e))
    {
        return Err(ManagementTokenError::InvalidToken);
    }
    Ok(())
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
