use serde_json::json;

use super::{ApiResponse, ConfigApiState};
use crate::config::coordinator::AuthoredRevision;

impl ConfigApiState {
    pub(super) fn promote_overlay_response(
        &self,
        expected: &AuthoredRevision,
        now_unix_ms: u64,
    ) -> ApiResponse {
        let Some(store) = self
            .generations
            .as_ref()
            .and_then(|manager| manager.overlay())
        else {
            return ApiResponse::error(
                503,
                "overlay_unavailable",
                "this process keeps no overlay beside a configuration file",
            );
        };
        let Ok(overlay) = store.load() else {
            return ApiResponse::error(
                500,
                "overlay_unavailable",
                "the overlay file could not be read; fix or clear it",
            );
        };
        let document = match self.coordinator.load_shared() {
            Ok(document) => document,
            Err(rejection) => return self.source_unavailable(&rejection),
        };
        let mut draft = document.validated_config.to_draft();
        let promoted = overlay.promote_capacities(&mut draft.upstream_pools);
        if promoted.is_empty() {
            return ApiResponse::json(
                200,
                &json!({ "outcome": "nothing_to_promote", "promoted": 0 }),
            );
        }
        let response = self.persist(expected, |mutation| {
            self.prepare_candidate(draft, now_unix_ms, mutation)
        });
        if response.status == 200
            && store
                .update(|overlay| overlay.without_max_connections(&promoted))
                .is_err()
        {
            return ApiResponse::error(
                500,
                "overlay_unavailable",
                "the configuration was saved but the overlay file could not be updated",
            );
        }
        response
    }
}
