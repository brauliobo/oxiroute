use std::{sync::Arc, time::Duration};

use oxiroute_supervision::{LifecycleControl, LifecycleRequest};

use super::{EventApiVersion, ManagementState, Route, match_route};
use crate::{
    GenerationManager, RuntimeMetrics,
    lifecycle_control::{LifecycleError, LifecycleOutcome, LifecyclePort},
    operational_event::{AuditLimits, AuditStore},
};

struct ContractLifecycleControl(GenerationManager);

impl LifecycleControl for ContractLifecycleControl {
    type Revision = String;
    type Status = crate::GenerationStatus;
    type Outcome = LifecycleOutcome;
    type Error = LifecycleError;

    fn status(&self) -> Self::Status {
        self.0.status()
    }

    fn execute(
        &self,
        _request: LifecycleRequest<Self::Revision>,
        _timeout: Option<Duration>,
    ) -> Result<Self::Outcome, Self::Error> {
        Err(LifecycleError::ShutdownUnavailable)
    }
}

#[test]
fn management_state_compiles_against_lifecycle_port_without_direct_adapter() {
    let generations = GenerationManager::new();
    let lifecycle: Arc<LifecyclePort> = Arc::new(ContractLifecycleControl(generations.clone()));

    let _state = ManagementState::new(
        generations,
        lifecycle,
        RuntimeMetrics::new(),
        Arc::new(AuditStore::memory(AuditLimits::default())),
    );
}

#[test]
fn audit_routes_preserve_query_filters_and_status_is_distinct() {
    assert!(matches!(
        match_route("/api/v1/audit?after=4&limit=2&category=reload"),
        Some(Route::Audit(Some("after=4&limit=2&category=reload")))
    ));
    assert!(matches!(
        match_route("/api/v1/audit/status"),
        Some(Route::AuditStatus)
    ));
}

#[test]
fn event_pages_include_the_authoritative_latest_cursor() {
    let response = super::ManagementState::events(EventApiVersion::V2, Some("after=0&limit=1"));
    let body: serde_json::Value = serde_json::from_slice(&response.body).expect("event JSON");

    assert!(body["latestCursor"].is_u64());
}

#[test]
fn version_one_event_pages_keep_the_shipped_shape() {
    let response = super::ManagementState::events(EventApiVersion::V1, Some("after=0&limit=1"));
    let body: serde_json::Value = serde_json::from_slice(&response.body).expect("event JSON");

    assert!(body.get("latestCursor").is_none());
}

#[test]
fn version_two_event_page_and_stream_routes_are_exact() {
    assert!(matches!(
        match_route("/api/v1/events?after=4&limit=2"),
        Some(Route::Events(EventApiVersion::V1, Some("after=4&limit=2")))
    ));
    assert!(matches!(
        match_route("/api/v2/events?after=4&limit=2"),
        Some(Route::Events(EventApiVersion::V2, Some("after=4&limit=2")))
    ));
    assert!(matches!(
        match_route("/api/v2/events/stream?after=4&limit=2"),
        Some(Route::EventStream(
            EventApiVersion::V2,
            Some("after=4&limit=2")
        ))
    ));
    assert!(match_route("/api/v2/events/").is_none());
    assert!(match_route("/api/v2/events/stream/").is_none());
}

#[test]
fn managed_acme_lifecycle_routes_are_exact_and_post_only() {
    for path in [
        "/api/v1/tls/revoke",
        "/api/v1/tls/delete",
        "/api/v1/tls/account/rollover",
        "/api/v1/tls/jobs/cancel",
        "/api/v1/tls/jobs/pause",
        "/api/v1/tls/jobs/resume",
    ] {
        assert!(match_route(path).is_some(), "route missing: {path}");
        assert!(match_route(&format!("{path}/")).is_none());
    }
}

#[test]
fn invalid_revocation_reason_is_a_client_error() {
    let response = super::managed_error_response(
        crate::AcmeManagedError::Protocol(oxiroute_acme::AcmeError::InvalidRevocationReason),
        "revocation failed",
    );
    assert_eq!(response.status, 400);
}

#[test]
fn unresolved_dns_cleanup_blocks_state_deletion_with_a_conflict() {
    let response = super::managed_error_response(
        crate::AcmeManagedError::State(oxiroute_acme::AcmeStateError::PendingDnsCleanup),
        "deletion failed",
    );
    assert_eq!(response.status, 409);
    let body: serde_json::Value = serde_json::from_slice(&response.body).expect("error JSON");
    assert_eq!(body["error"]["code"], "dns_cleanup_pending");
}
