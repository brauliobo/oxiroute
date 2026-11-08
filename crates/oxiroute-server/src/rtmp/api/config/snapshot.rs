use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{ApiResponse, RedactedConfigView};
use crate::config::coordinator::{
    AuthoredRevision, CanonicalConfigCoordinator, EffectiveRevision, ResolvedConfigDocument,
};

use super::super::dto::ConfigSnapshotResponse;

/// The `GET /config` body for one persisted document and active revision.
///
/// Rendering it validates and serializes the whole configuration, so it is built once per change
/// and served with a validator that lets a poller skip the body when nothing changed.
#[derive(Default)]
pub(super) struct SnapshotCache {
    cached: Mutex<Option<CachedSnapshot>>,
}

struct CachedSnapshot {
    disk: AuthoredRevision,
    active: EffectiveRevision,
    body: Arc<[u8]>,
}

impl SnapshotCache {
    /// Reads the persisted file (blocking) and answers the request for it.
    pub(super) fn response(
        &self,
        coordinator: &CanonicalConfigCoordinator,
        active: &EffectiveRevision,
        if_none_match: Option<&str>,
    ) -> ApiResponse {
        let document = match coordinator.load_shared() {
            Ok(document) => document,
            Err(rejection) => {
                return ApiResponse::json(
                    503,
                    &json!({
                        "schemaVersion": 1,
                        "diskRevision": rejection.disk_revision,
                        "activeRevision": active,
                        "diagnostics": rejection.diagnostics,
                        "error": {
                            "code": "canonical_config_unavailable",
                            "message": "the persisted canonical configuration could not be loaded",
                        },
                    }),
                );
            }
        };
        let etag = etag(&document.authored_revision, active);
        if if_none_match.is_some_and(|header| matches_validator(header, &etag)) {
            return ApiResponse::not_modified(etag);
        }
        self.body(&document, active).with_etag(etag)
    }

    fn body(&self, document: &ResolvedConfigDocument, active: &EffectiveRevision) -> ApiResponse {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = cached
            .as_ref()
            .filter(|hit| hit.disk == document.authored_revision && hit.active == *active)
        {
            return ApiResponse::bytes(200, hit.body.to_vec(), "application/json");
        }
        let view = RedactedConfigView::of(
            &document.validated_config,
            document.format,
            &document.config_preview,
        );
        let response = ApiResponse::dto(
            200,
            &ConfigSnapshotResponse::new(document, active.clone(), view),
        );
        *cached = Some(CachedSnapshot {
            disk: document.authored_revision.clone(),
            active: active.clone(),
            body: Arc::from(response.body.as_slice()),
        });
        response
    }
}

/// The body is a function of the persisted bytes and the active revision, nothing else.
fn etag(disk: &AuthoredRevision, active: &EffectiveRevision) -> String {
    format!("\"{disk}-{active}\"")
}

/// Weak comparison of an `If-None-Match` list (RFC 9110, section 13.1.2).
fn matches_validator(header: &str, etag: &str) -> bool {
    header.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_none_match_uses_weak_comparison_over_a_list() {
        let etag = "\"a-b\"";
        assert!(matches_validator("\"a-b\"", etag));
        assert!(matches_validator("W/\"a-b\"", etag));
        assert!(matches_validator("\"x\", \"a-b\"", etag));
        assert!(matches_validator("*", etag));
        assert!(!matches_validator("\"a-c\"", etag));
        assert!(!matches_validator("", etag));
    }
}
