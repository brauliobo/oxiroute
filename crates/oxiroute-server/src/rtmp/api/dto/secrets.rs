use oxiroute_rtmp::{RtmpSecretFingerprint, RtmpSecretRole};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RtmpSecretRoleDto {
    Publish,
    Play,
}

impl From<RtmpSecretRole> for RtmpSecretRoleDto {
    fn from(role: RtmpSecretRole) -> Self {
        match role {
            RtmpSecretRole::Publish => Self::Publish,
            RtmpSecretRole::Play => Self::Play,
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpSecretFingerprintDto {
    service: String,
    application: String,
    role: RtmpSecretRoleDto,
    fingerprint: String,
}

impl From<&RtmpSecretFingerprint> for RtmpSecretFingerprintDto {
    fn from(entry: &RtmpSecretFingerprint) -> Self {
        Self {
            service: entry.service.clone(),
            application: entry.application.clone(),
            role: entry.role.into(),
            fingerprint: entry.fingerprint.clone(),
        }
    }
}

#[derive(JsonSchema, Serialize)]
pub(crate) struct RtmpSecretsResponse {
    secrets: Vec<RtmpSecretFingerprintDto>,
}

impl RtmpSecretsResponse {
    pub(crate) fn project(fingerprints: &[RtmpSecretFingerprint]) -> Self {
        Self {
            secrets: fingerprints.iter().map(Into::into).collect(),
        }
    }
}
