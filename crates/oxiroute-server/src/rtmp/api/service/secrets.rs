#[allow(clippy::wildcard_imports)]
use super::*;
use crate::rtmp::api::dto::RtmpSecretsResponse;

impl RtmpManagementApi {
    pub(super) fn rtmp_secret_fingerprints(&self, method: &str) -> ApiResponse {
        if method != "GET" {
            return ApiResponse::method_not_allowed("GET");
        }
        let control = self
            .generations
            .as_ref()
            .and_then(GenerationManager::active)
            .map_or_else(
                || self.control.clone(),
                |generation| generation.rtmp_control(),
            );
        ApiResponse::dto(
            200,
            &RtmpSecretsResponse::project(&control.secret_fingerprints()),
        )
        .with_cache_control("no-store")
    }
}
