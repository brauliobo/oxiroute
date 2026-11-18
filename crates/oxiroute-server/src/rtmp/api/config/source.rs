use std::sync::Arc;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config};
use pingora::protocols::http::ServerSession;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ApiResponse, ConfigApiState, PreparedCandidate, Route, config_revision_header,
    diagnostics_json, listener_changes_diagnostic, read_config_body, require_json_content_type,
    restart_required_diagnostic,
};
use crate::{
    GenerationManager, RuntimeGeneration,
    config::{
        coordinator::{AuthoredRevision, ConfigLoadRejection, ResolvedConfigDocument},
        diff::unified,
    },
    rtmp::api::response::system_time_ms,
};
use comparison::comparison;
use redaction::SourceSecrets;

mod comparison;
mod history;
mod redaction;

pub(super) use history::{HistoryRoute, history_route};

const REDACTION_POLICY: &str = "rtmp_inline_secrets";

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceFormat {
    Kdl,
    Lua,
    Hocon,
    Uci,
    Json,
}

impl SourceFormat {
    const fn config_format(self) -> ConfigFormat {
        match self {
            Self::Kdl => ConfigFormat::Kdl,
            Self::Lua => ConfigFormat::Lua,
            Self::Hocon | Self::Json => ConfigFormat::Hocon,
            Self::Uci => ConfigFormat::Uci,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceRequest {
    #[serde(default)]
    format: Option<SourceFormat>,
    text: String,
    #[serde(default)]
    text_diff: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SourceView {
    Disk,
    Active,
}

struct Authority {
    disk: Result<Arc<ResolvedConfigDocument>, ConfigLoadRejection>,
    active: Option<Arc<RuntimeGeneration>>,
}

impl Authority {
    fn drafts(&self) -> Vec<&ConfigDraft> {
        let disk = self
            .disk
            .as_ref()
            .ok()
            .map(|document| document.validated_config.as_draft());
        let active = self
            .active
            .as_ref()
            .map(|generation| generation.config().as_draft());
        disk.into_iter().chain(active).collect()
    }

    fn secrets(&self) -> SourceSecrets {
        SourceSecrets::of(&self.drafts())
    }

    fn secrets_with(&self, candidate: &ConfigDraft) -> SourceSecrets {
        let mut drafts = self.drafts();
        drafts.push(candidate);
        SourceSecrets::of(&drafts)
    }

    fn secrets_with_all(&self, candidates: &[&ConfigDraft]) -> SourceSecrets {
        let mut drafts = self.drafts();
        drafts.extend_from_slice(candidates);
        SourceSecrets::of(&drafts)
    }

    fn restored(&self, text: &str) -> Result<String, ApiResponse> {
        self.secrets().restore(text).map_err(|_| {
            ApiResponse::error(
                422,
                "redacted_secret_unresolved",
                "a redacted placeholder does not match a secret of the stored configuration",
            )
        })
    }
}

impl ConfigApiState {
    pub(super) async fn handle_source(
        &self,
        route: Route,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        match (route, method) {
            (Route::Source, "GET") => match source_view(session.req_header().uri.query()) {
                Ok(view) => self.source_response(view),
                Err(response) => response,
            },
            (Route::Source, "PUT") => {
                let revision = match config_revision_header(session) {
                    Ok(revision) => revision,
                    Err(response) => return response,
                };
                match read_source_request(session).await {
                    Ok(request) => self.save_source_response(&revision, &request),
                    Err(response) => response,
                }
            }
            (Route::SourceValidate, "POST") => match read_source_request(session).await {
                Ok(request) => self.validate_source_response(&request),
                Err(response) => response,
            },
            (Route::Source, _) => ApiResponse::method_not_allowed("GET, PUT"),
            _ => ApiResponse::method_not_allowed("POST"),
        }
    }

    fn authority(&self) -> Authority {
        Authority {
            disk: self.coordinator.load_shared(),
            active: self
                .generations
                .as_ref()
                .and_then(GenerationManager::active),
        }
    }

    fn source_response(&self, view: SourceView) -> ApiResponse {
        let source = match self.coordinator.read_source() {
            Ok(source) => source,
            Err(rejection) => return self.source_unavailable(&rejection),
        };
        let authority = self.authority();
        let secrets = authority.secrets();
        let text = match (view, &authority.active) {
            (SourceView::Disk, _) => source.text.clone(),
            (SourceView::Active, Some(active)) => {
                match render_config(source.format, active.config()) {
                    Ok(text) => text,
                    Err(_) => {
                        return ApiResponse::error(
                            500,
                            "active_config_render_failed",
                            "the active configuration could not be rendered",
                        );
                    }
                }
            }
            (SourceView::Active, None) => {
                return ApiResponse::error(
                    503,
                    "active_config_unavailable",
                    "no configuration generation is active",
                );
            }
        };
        let withheld = !secrets.can_redact();
        let redacted = secrets.redact(&text);
        let (valid, candidate, diagnostics) = match &authority.disk {
            Ok(document) => (true, Some(&document.effective_revision), Vec::new()),
            Err(rejection) => (false, None, rejection.diagnostics.clone()),
        };
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "view": if view == SourceView::Disk { "disk" } else { "active" },
                "authored": view == SourceView::Disk,
                "format": source.format,
                "diskRevision": source.revision,
                "candidateRevision": candidate,
                "activeRevision": self.active_revision(),
                "valid": valid,
                "diagnostics": diagnostics,
                "text": if withheld { None } else { Some(redacted.text) },
                "redaction": {
                    "policy": REDACTION_POLICY,
                    "redacted": redacted.replaced,
                    "withheld": withheld,
                },
                "configWritable": self.coordinator.write_block().is_none(),
                "writeBlock": self.coordinator.write_block(),
            }),
        )
    }

    fn source_unavailable(&self, rejection: &ConfigLoadRejection) -> ApiResponse {
        ApiResponse::json(
            503,
            &json!({
                "schemaVersion": 1,
                "diskRevision": rejection.disk_revision,
                "activeRevision": self.active_revision(),
                "diagnostics": rejection.diagnostics,
                "error": {
                    "code": "config_source_unavailable",
                    "message": "the configuration file could not be read",
                },
            }),
        )
    }

    fn save_source_response(
        &self,
        expected: &AuthoredRevision,
        request: &SourceRequest,
    ) -> ApiResponse {
        let now_unix_ms = match system_time_ms() {
            Ok(now) => now,
            Err(response) => return response,
        };
        let text = match self.authority().restored(&request.text) {
            Ok(text) => text,
            Err(response) => return response,
        };
        let format = request
            .format
            .map_or_else(|| self.coordinator.format(), SourceFormat::config_format);
        self.persist(expected, |mutation| {
            self.preflight(
                self.coordinator.prepare_source(&text, format),
                now_unix_ms,
                mutation,
            )
        })
    }

    fn validate_source_response(&self, request: &SourceRequest) -> ApiResponse {
        let now_unix_ms = match system_time_ms() {
            Ok(now) => now,
            Err(response) => return response,
        };
        let authority = self.authority();
        let text = match authority.restored(&request.text) {
            Ok(text) => text,
            Err(response) => return response,
        };
        let format = request
            .format
            .map_or_else(|| self.coordinator.format(), SourceFormat::config_format);
        let outcome = self.coordinator.prepare_source(&text, format);
        let prepared = match self.preflight(outcome, now_unix_ms, None) {
            Ok(prepared) => prepared,
            Err(response) => return response,
        };
        let body = self.validation_body(&authority, &prepared, &text, request.text_diff);
        ApiResponse::json(200, &body)
    }

    fn validation_body(
        &self,
        authority: &Authority,
        prepared: &PreparedCandidate,
        text: &str,
        text_diff: bool,
    ) -> Value {
        let mut diagnostics = diagnostics_json(prepared.draft.diagnostics(), false);
        if !prepared.startup_only.is_empty() {
            diagnostics.extend(
                prepared
                    .startup_only
                    .iter()
                    .map(restart_required_diagnostic),
            );
        } else if let Some(changes) = &prepared.listener_changes {
            diagnostics.push(listener_changes_diagnostic(self.mode, changes));
        }
        let matches_disk = prepared.draft.format() == self.coordinator.format();
        json!({
            "schemaVersion": 1,
            "valid": true,
            "format": prepared.draft.format(),
            "matchesDiskFormat": matches_disk,
            "candidateRevision": prepared.draft.effective_revision(),
            "activeRevision": self.active_revision(),
            "compositional": prepared.draft.compositional(),
            "dependencyCount": prepared.draft.dependencies().len(),
            "diagnostics": diagnostics,
            "restartRequired": !prepared.startup_only.is_empty(),
            "restartReasons": prepared.startup_only,
            "topology": prepared.topology,
            "diff": comparisons(authority, prepared.draft.validated_config()),
            "textDiff": (text_diff && matches_disk)
                .then(|| self.text_diff(authority, prepared.draft.validated_config(), text))
                .flatten(),
            "configWritable": self.coordinator.write_block().is_none(),
            "writeBlock": self.coordinator.write_block(),
        })
    }

    fn text_diff(
        &self,
        authority: &Authority,
        candidate: &ValidatedConfig,
        text: &str,
    ) -> Option<String> {
        let disk = self.coordinator.read_source().ok()?;
        let secrets = authority.secrets_with(candidate.as_draft());
        let old = secrets.redact(&disk.text).text;
        let new = secrets.redact(text).text;
        Some(unified(&old, &new, "disk", "candidate"))
    }
}

fn comparisons(authority: &Authority, candidate: &ValidatedConfig) -> Value {
    let candidate = config_value(candidate);
    let disk = authority
        .disk
        .as_ref()
        .ok()
        .map(|document| (document, config_value(&document.validated_config)));
    let active = authority
        .active
        .as_ref()
        .map(|generation| config_value(generation.config()));
    json!({
        "disk": disk.as_ref().map(|(document, disk)| {
            comparison(Some(document.authored_revision.as_str()), disk, &candidate)
        }),
        "active": active.as_ref().map(|active| comparison(None, active, &candidate)),
        "pending": disk.as_ref().zip(active.as_ref()).map(|((_, disk), active)| {
            comparison(None, active, disk)
        }),
    })
}

fn config_value(config: &ValidatedConfig) -> Value {
    serde_json::to_value(config).expect("validated configuration serializes")
}

fn source_view(query: Option<&str>) -> Result<SourceView, ApiResponse> {
    let view = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .find_map(|pair| pair.strip_prefix("view="));
    match view {
        None | Some("disk") => Ok(SourceView::Disk),
        Some("active") => Ok(SourceView::Active),
        Some(_) => Err(ApiResponse::error(
            400,
            "invalid_source_view",
            "view must be disk or active",
        )),
    }
}

async fn read_source_request(session: &mut ServerSession) -> Result<SourceRequest, ApiResponse> {
    require_json_content_type(session)?;
    let body = read_config_body(session).await?;
    serde_json::from_slice(&body).map_err(|_| {
        ApiResponse::error(
            400,
            "invalid_source_request",
            "body must be a JSON object with a text field and an optional format and textDiff",
        )
    })
}
