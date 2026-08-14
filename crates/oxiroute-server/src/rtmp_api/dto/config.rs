use oxiroute_config::{ConfigDraft, RtmpAccessPolicy, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_coordinator::{
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
        document: ResolvedConfigDocument,
        active_revision: EffectiveRevision,
        view: RedactedConfigView,
    ) -> Self {
        let (config, config_preview, lua_preview) = view.into_parts();
        Self {
            schema_version: 1,
            disk_revision: document.authored_revision,
            candidate_revision: document.effective_revision,
            active_revision,
            config,
            config_format: document.format,
            compositional: document.compositional,
            dependency_count: document.dependencies.len(),
            config_preview,
            lua_preview,
            diagnostics: document.diagnostics,
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
mod tests {
    use super::*;
    use oxiroute_config::{
        RtmpApplication, RtmpAutoPushPolicy, RtmpCallbackConfig, RtmpFanoutPolicy,
        RtmpOutboundPolicy, RtmpRelayPolicy, RtmpService, RtmpSessionCeilings, RtmpTokenPolicy,
        RtmpTokenSource,
    };
    use serde_json::json;

    fn config() -> ConfigDraft {
        serde_json::from_value(json!({ "version": 1, "listeners": [] }))
            .expect("minimal configuration")
    }

    fn config_with_token(secret: &str) -> ConfigDraft {
        let mut config = config();
        config.rtmp_services.push(RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            max_inbound_message_size: 8 * 1_024 * 1_024,
            ack_window_size: 5_000_000,
            access_log: None,
            outbound_policy: RtmpOutboundPolicy::default(),
            callbacks: RtmpCallbackConfig::default(),
            auto_push: RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![RtmpApplication {
                name: "broadcast".into(),
                live: true,
                idle_streams: true,
                publish: RtmpAccessPolicy {
                    rules: Vec::new(),
                    token: Some(RtmpTokenPolicy {
                        source: RtmpTokenSource::StreamQuery,
                        parameter: "token".into(),
                        secret: secret.into(),
                    }),
                },
                play: RtmpAccessPolicy::default(),
                limits: RtmpSessionCeilings::default(),
                push_targets: Vec::new(),
                pull_targets: Vec::new(),
                relay: RtmpRelayPolicy::default(),
                callbacks: RtmpCallbackConfig::default(),
                fanout: RtmpFanoutPolicy::default(),
                vod: None,
                hls: None,
                dash: None,
                recorders: Vec::new(),
            }],
        });
        config
    }

    #[test]
    fn request_is_strict_and_distinct_from_the_redacted_view() {
        let request: ConfigRequest = serde_json::from_value(json!({
            "config": { "version": 1, "listeners": [] }
        }))
        .expect("typed request");
        assert_eq!(request.into_config(), config());
        assert!(
            serde_json::from_value::<ConfigRequest>(json!({
                "config": { "version": 1, "listeners": [] },
                "normalizedConfig": { "version": 1, "listeners": [] }
            }))
            .is_err()
        );
    }

    #[test]
    fn redacted_view_preserves_the_sentinel_and_lua_alias_without_the_secret() {
        let validated = config_with_token("private-token").validate().unwrap();
        let (redacted, preview, lua_preview) =
            RedactedConfigView::new(&validated, ConfigFormat::Lua).into_parts();
        let redacted = serde_json::to_value(redacted).unwrap();

        assert_eq!(
            redacted["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
            "<redacted>"
        );
        assert_eq!(lua_preview.as_deref(), Some(preview.as_str()));
        assert!(!preview.contains("private-token"));
    }

    #[test]
    fn sentinel_merge_restores_only_the_matching_authoritative_token() {
        let authoritative = config_with_token("private-token");
        let mut round_trip = config_with_token("<redacted>");

        assert!(contains_redacted_rtmp_token_secret(&round_trip));
        restore_redacted_rtmp_token_secrets(&mut round_trip, &authoritative);
        assert_eq!(
            round_trip.rtmp_services[0].applications[0]
                .publish
                .token
                .as_ref()
                .unwrap()
                .secret,
            "private-token"
        );

        round_trip.rtmp_services[0].applications[0]
            .publish
            .token
            .as_mut()
            .unwrap()
            .secret = "replacement-token".into();
        restore_redacted_rtmp_token_secrets(&mut round_trip, &authoritative);
        assert_eq!(
            round_trip.rtmp_services[0].applications[0]
                .publish
                .token
                .as_ref()
                .unwrap()
                .secret,
            "replacement-token"
        );
    }

    #[test]
    fn snapshot_dto_has_the_exact_top_level_json_contract() {
        let validated = config().validate().unwrap();
        let document = ResolvedConfigDocument {
            authored_revision: AuthoredRevision::from_bytes(b"disk"),
            effective_revision: EffectiveRevision::from_bytes(b"candidate"),
            validated_config: validated.clone(),
            format: ConfigFormat::Kdl,
            compositional: false,
            dependencies: Vec::new(),
            config_preview: "ignored unredacted preview".into(),
            diagnostics: Vec::new(),
        };
        let response = ConfigSnapshotResponse::new(
            document,
            EffectiveRevision::from_bytes(b"active"),
            RedactedConfigView::new(&validated, ConfigFormat::Kdl),
        );
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "activeRevision",
                "candidateRevision",
                "compositional",
                "config",
                "configFormat",
                "configPreview",
                "dependencyCount",
                "diagnostics",
                "diskRevision",
                "schemaVersion",
            ]
        );
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["config"], serde_json::to_value(validated).unwrap());
        assert!(value.get("luaPreview").is_none());
    }
}
