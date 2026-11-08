use oxiroute_config::{ConfigDraft, RtmpAccessPolicy, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::coordinator::{
    AuthoredRevision, ConfigDiagnostic, EffectiveRevision, ResolvedConfigDocument,
};

const REDACTED_RTMP_TOKEN: &str = "<redacted>";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigRequest {
    config: ConfigDraft,
}

impl ConfigRequest {
    pub(crate) fn into_config(self) -> ConfigDraft {
        self.config
    }
}

pub(crate) struct RedactedConfigView {
    config: ValidatedConfig,
    preview: String,
    lua_preview: Option<String>,
}

impl RedactedConfigView {
    /// The view of a configuration whose `preview` is already rendered in `format`.
    ///
    /// Without RTMP tokens there is nothing to redact, so the config and preview are reused
    /// instead of being validated and rendered a second time.
    pub(crate) fn of(config: &ValidatedConfig, format: ConfigFormat, preview: &str) -> Self {
        if !has_rtmp_token(config.as_draft()) {
            return Self {
                config: config.clone(),
                preview: preview.to_owned(),
                lua_preview: (format == ConfigFormat::Lua).then(|| preview.to_owned()),
            };
        }
        Self::new(config, format)
    }

    pub(crate) fn new(config: &ValidatedConfig, format: ConfigFormat) -> Self {
        let mut config = config.to_draft();
        for service in &mut config.rtmp_services {
            for application in &mut service.applications {
                for policy in [&mut application.publish, &mut application.play] {
                    if let Some(token) = policy.token.as_mut() {
                        token.secret = REDACTED_RTMP_TOKEN.into();
                    }
                }
            }
        }
        let config = config
            .validate()
            .expect("normalized configuration remains valid after token redaction");
        let preview = render_config(format, &config)
            .expect("normalized configuration remains renderable after token redaction");
        let lua_preview = (format == ConfigFormat::Lua).then(|| preview.clone());
        Self {
            config,
            preview,
            lua_preview,
        }
    }

    pub(crate) fn into_parts(self) -> (ValidatedConfig, String, Option<String>) {
        (self.config, self.preview, self.lua_preview)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigSnapshotResponse {
    schema_version: u8,
    disk_revision: AuthoredRevision,
    candidate_revision: EffectiveRevision,
    active_revision: EffectiveRevision,
    config: ValidatedConfig,
    config_format: ConfigFormat,
    compositional: bool,
    dependency_count: usize,
    config_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lua_preview: Option<String>,
    diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigSnapshotResponse {
    pub(crate) fn new(
        document: &ResolvedConfigDocument,
        active_revision: EffectiveRevision,
        view: RedactedConfigView,
    ) -> Self {
        let (config, config_preview, lua_preview) = view.into_parts();
        Self {
            schema_version: 1,
            disk_revision: document.authored_revision.clone(),
            candidate_revision: document.effective_revision.clone(),
            active_revision,
            config,
            config_format: document.format,
            compositional: document.compositional,
            dependency_count: document.dependencies.len(),
            config_preview,
            lua_preview,
            diagnostics: document.diagnostics.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigValidationResponse {
    candidate_revision: EffectiveRevision,
    normalized_config: ValidatedConfig,
    config_format: ConfigFormat,
    compositional: bool,
    dependency_count: usize,
    config_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lua_preview: Option<String>,
    diagnostics: Vec<Value>,
    topology: Value,
    restart_required: bool,
}

impl ConfigValidationResponse {
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor mirrors the fixed wire schema"
    )]
    pub(crate) fn new(
        candidate_revision: EffectiveRevision,
        format: ConfigFormat,
        compositional: bool,
        dependency_count: usize,
        view: RedactedConfigView,
        diagnostics: Vec<Value>,
        topology: Value,
        restart_required: bool,
    ) -> Self {
        let (normalized_config, config_preview, lua_preview) = view.into_parts();
        Self {
            candidate_revision,
            normalized_config,
            config_format: format,
            compositional,
            dependency_count,
            config_preview,
            lua_preview,
            diagnostics,
            topology,
            restart_required,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigConflictResponse {
    schema_version: u8,
    disk_revision: AuthoredRevision,
    candidate_revision: EffectiveRevision,
    active_revision: EffectiveRevision,
    expected_revision: AuthoredRevision,
    outcome: &'static str,
    config: ValidatedConfig,
    config_format: ConfigFormat,
    compositional: bool,
    dependency_count: usize,
    config_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lua_preview: Option<String>,
    diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigConflictResponse {
    pub(crate) fn new(
        document: ResolvedConfigDocument,
        active_revision: EffectiveRevision,
        expected_revision: AuthoredRevision,
        diagnostics: Vec<ConfigDiagnostic>,
        view: RedactedConfigView,
    ) -> Self {
        let (config, config_preview, lua_preview) = view.into_parts();
        Self {
            schema_version: 1,
            disk_revision: document.authored_revision,
            candidate_revision: document.effective_revision,
            active_revision,
            expected_revision,
            outcome: "conflict",
            config,
            config_format: document.format,
            compositional: document.compositional,
            dependency_count: document.dependencies.len(),
            config_preview,
            lua_preview,
            diagnostics,
        }
    }
}

fn has_rtmp_token(config: &ConfigDraft) -> bool {
    config.rtmp_services.iter().any(|service| {
        service.applications.iter().any(|application| {
            [&application.publish, &application.play]
                .into_iter()
                .any(|policy| policy.token.is_some())
        })
    })
}

pub(crate) fn contains_redacted_rtmp_token_secret(config: &ConfigDraft) -> bool {
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

pub(crate) fn restore_redacted_rtmp_token_secrets(
    draft: &mut ConfigDraft,
    authoritative: &ConfigDraft,
) {
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

#[cfg(test)]
mod tests;
