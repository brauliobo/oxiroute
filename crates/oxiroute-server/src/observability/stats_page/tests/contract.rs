//! A snapshot the proxy really builds must satisfy the published schema: every key it emits is
//! declared, every required key is present and the scalar types agree.

use std::time::Duration;

use serde_json::{Value, json};

use super::fixture::{API, fixture};
use crate::{
    HttpOperationResult,
    observability::stats_page::{builder::build_snapshot, contract::generated_openapi, model::*},
};

fn check(schemas: &Value, schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema["$ref"].as_str() {
        let name = reference.rsplit('/').next().unwrap();
        return check(schemas, &schemas[name], value, path, errors);
    }
    let mut fail = |message: String| errors.push(format!("{path}: {message}"));
    match schema["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return fail("not an object".into());
            };
            let properties = schema["properties"].as_object();
            for (key, member) in object {
                match (
                    properties.and_then(|p| p.get(key)),
                    schema.get("additionalProperties"),
                ) {
                    (Some(declared), _) | (None, Some(declared)) => {
                        check(schemas, declared, member, &format!("{path}.{key}"), errors);
                    }
                    (None, None) => errors.push(format!("{path}.{key}: undeclared key")),
                }
            }
            for required in schema["required"].as_array().into_iter().flatten() {
                if !object.contains_key(required.as_str().unwrap()) {
                    errors.push(format!("{path}: missing required {required}"));
                }
            }
        }
        Some("array") => match value.as_array() {
            Some(items) => items.iter().enumerate().for_each(|(index, item)| {
                check(
                    schemas,
                    &schema["items"],
                    item,
                    &format!("{path}[{index}]"),
                    errors,
                );
            }),
            None => fail("not an array".into()),
        },
        Some("string") => match value.as_str() {
            None => fail("not a string".into()),
            Some(text) => {
                if let Some(allowed) = schema["enum"].as_array()
                    && !allowed.contains(&json!(text))
                {
                    fail(format!("{text:?} is not one of {allowed:?}"));
                }
                if schema["pattern"] == "^[0-9]+$" && !text.bytes().all(|b| b.is_ascii_digit()) {
                    fail(format!("{text:?} is not a decimal counter"));
                }
            }
        },
        Some("integer") if !(value.is_u64() || value.is_i64()) => fail("not an integer".into()),
        Some("number") if !value.is_number() => fail("not a number".into()),
        Some("boolean") if !value.is_boolean() => fail("not a boolean".into()),
        _ => {}
    }
}

fn populated() -> StatsSnapshot {
    let fixture = fixture();
    let front = fixture
        .generation
        .traffic_listener_metrics("front")
        .unwrap();
    front
        .record_http_operation(HttpOperationResult::Success, Duration::from_millis(4))
        .unwrap();
    front.record_rescue("0", 502);
    front.record_pool_selection("1", API);
    let mut snapshot = build_snapshot(&fixture.generations, 1_000).unwrap();
    snapshot.generation.last_rejection = Some(RejectionStats {
        at_unix_ms: 1,
        revision: Some("abc".into()),
        diagnostics: vec![DiagnosticStats {
            code: "E_X".into(),
            stage: "parse".into(),
            message: "m".into(),
        }],
    });
    snapshot.generation.retired.push(RetiredGeneration {
        revision: "old".into(),
        state: RetiredState::Draining,
        age_seconds: 3,
        deadline_remaining_seconds: 7,
        in_flight: 2,
        in_flight_by_kind: [("http".to_owned(), 2)].into(),
    });
    snapshot.certificates.push(CertificateStats {
        name: "edge".into(),
        source: CertificateSource::AcmeManaged,
        expires_at: "Jan  5 12:00:00 2030 GMT".into(),
        not_after_unix_seconds: Some(1_893_844_800),
        next_action_unix_seconds: Some(1),
        last_outcome: Some("renewed".into()),
        last_error_code: Some("E".into()),
    });
    snapshot.certificate_watchers.push(CertificateWatcherStats {
        source: CertificateSource::Certbot,
        health: CertbotWatcherHealth::Healthy,
        backend_errors: 0.into(),
        reconciliation_failures: 0.into(),
    });
    let pool = &mut snapshot.pools[0];
    pool.queue.by_class = Some(vec![ClassCount {
        class: "gold".into(),
        count: 1,
    }]);
    pool.canary = Some(CanaryStats {
        result: Some(CanaryResult::Ok),
        reason: Some(HealthFailure::Slow),
        server: Some("origin-a".into()),
        duration_ms: 3,
        last_attempt_unix_ms: Some(1),
        last_success_unix_ms: Some(1),
        consecutive_failures: 0,
        interval_ms: 1_000,
    });
    let server = &mut pool.servers[0];
    server.last_transition_at_unix_ms = Some(1);
    server.downtime_ms = Some(1);
    server.check.last_checked_at_unix_ms = Some(1);
    server.check.last_failure = Some(HealthFailure::Timeout);
    server.passive.ejection_reason = Some(HealthFailure::ConnectFailed);
    snapshot
}

#[test]
fn a_populated_snapshot_satisfies_the_published_schema() {
    let document = generated_openapi();
    let schemas = &document["components"]["schemas"];
    for admin in [false, true] {
        let snapshot = populated();
        let value = serde_json::to_value(StatsDocument {
            admin,
            snapshot: &snapshot,
        })
        .unwrap();
        let mut errors = Vec::new();
        check(schemas, &schemas["StatsDocument"], &value, "$", &mut errors);
        assert!(errors.is_empty(), "{errors:#?}");
    }
}

#[test]
fn the_validator_notices_drift() {
    let document = generated_openapi();
    let schemas = &document["components"]["schemas"];
    let snapshot = populated();
    let mut value = serde_json::to_value(StatsDocument {
        admin: true,
        snapshot: &snapshot,
    })
    .unwrap();
    value["pools"][0]["servers"][0]["surprise"] = json!(1);
    value["pools"][0]["servers"][0]["traffic"]["bytesSent"] = json!(5);
    value["process"].as_object_mut().unwrap().remove("version");
    let mut errors = Vec::new();
    check(schemas, &schemas["StatsDocument"], &value, "$", &mut errors);
    assert_eq!(errors.len(), 3, "{errors:#?}");
}

#[test]
fn the_form_and_error_shapes_are_published() {
    let document = generated_openapi();
    let schemas = &document["components"]["schemas"];
    assert_eq!(
        schemas["AdminForm"]["required"],
        json!(["generation_revision", "pool", "server", "state"])
    );
    assert_eq!(
        schemas["AdminForm"]["properties"]["state"],
        json!({"$ref": "#/components/schemas/AdministrativeState"})
    );
    assert_eq!(
        schemas["AdministrativeState"]["enum"],
        json!(["ready", "drain", "maintenance"])
    );
    assert!(schemas["ErrorResponse"]["properties"]["error"].is_object());
}
