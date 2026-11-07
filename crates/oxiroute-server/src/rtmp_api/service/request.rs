#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn request_parts(session: &ServerSession) -> (String, String, String) {
    let request = session.req_header();
    (
        request.method.as_str().to_owned(),
        request.uri.path().to_owned(),
        request
            .uri
            .path_and_query()
            .map_or_else(|| request.uri.path().to_owned(), ToString::to_string),
    )
}

pub(super) fn request_context(session: &ServerSession) -> Result<AuditContext, ApiResponse> {
    match single_header(&session.req_header().headers, &CORRELATION_ID) {
        HeaderCardinality::Missing => {
            let mut context = AuditContext::generated();
            context.actor = "management_bearer".into();
            context.source = "management_api".into();
            Ok(context)
        }
        HeaderCardinality::Duplicate => Err(ApiResponse::error(
            400,
            "duplicate_correlation_id",
            "multiple correlation IDs are not accepted",
        )),
        HeaderCardinality::Single(value) => value
            .to_str()
            .ok()
            .and_then(AuditContext::from_external)
            .ok_or_else(|| {
                ApiResponse::error(
                    400,
                    "invalid_correlation_id",
                    "correlation ID must be 1 to 64 safe ASCII characters",
                )
            }),
    }
}

pub(super) fn audited_api_operation(
    method: &str,
    path: &str,
) -> Option<(&'static str, AuditCategory)> {
    if method == "GET" {
        return None;
    }
    if crate::registry::http::match_route(path).is_some() {
        return Some(("server_control", AuditCategory::Control));
    }
    let (operation, category) = match path {
        "/api/v1/config" => ("configuration_reload", AuditCategory::Reload),
        "/api/v1/generations/reload" => ("generation_reload", AuditCategory::Reload),
        "/api/v1/generations/rollback" => ("generation_rollback", AuditCategory::Reload),
        "/api/v1/generations/drain" => ("generation_drain", AuditCategory::Reload),
        "/api/v1/tls/reconcile" => ("certificate_reconcile", AuditCategory::Certificate),
        "/api/v1/tls/renew" => ("certificate_renew", AuditCategory::Certificate),
        "/api/v1/tls/revoke" => ("certificate_revoke", AuditCategory::Certificate),
        "/api/v1/tls/delete" => ("certificate_delete", AuditCategory::Certificate),
        "/api/v1/tls/account/rollover" => ("account_key_rollover", AuditCategory::Certificate),
        "/api/v1/tls/jobs/cancel" | "/api/v1/tls/jobs/pause" | "/api/v1/tls/jobs/resume" => {
            ("certificate_job_control", AuditCategory::Certificate)
        }
        "/api/v1/process/drain" => ("process_drain", AuditCategory::Control),
        "/api/v1/process/shutdown" => ("process_shutdown", AuditCategory::Control),
        "/api/v1/listeners/administrative-state" => ("listener_control", AuditCategory::Control),
        "/api/v1/pools/administrative-state" => ("pool_control", AuditCategory::Control),
        "/api/v1/servers/administrative-state"
        | "/api/v1/servers/health-override"
        | "/api/v1/servers/checks"
        | "/api/v1/servers/max-connections"
        | "/api/v1/servers/weight" => ("server_control", AuditCategory::Control),
        "/api/v1/servers/refresh-dns" => ("server_dns_refresh", AuditCategory::Control),
        _ => return None,
    };
    Some((operation, category))
}

pub(super) const fn audit_result_for_status(status: u16) -> AuditResult {
    match status {
        202 => AuditResult::Requested,
        207 => AuditResult::Partial,
        200..=299 => AuditResult::Succeeded,
        409 => AuditResult::Conflict,
        400..=499 => AuditResult::Rejected,
        _ => AuditResult::Failed,
    }
}
