use serde_json::{Value, json};

use super::*;

fn pool(name: &str, available: u64, total: u64) -> Value {
    json!({
        "name": name, "state": "ok", "available": available, "total": total, "ejected": 0,
        "failOpen": false, "queued": 0, "oldestQueueWaitMs": 0, "inflight": 0, "oldestInflightMs": 0,
    })
}

fn summary(pools: Vec<Value>, extra: Value) -> StatusSummary {
    let mut body = json!({
        "state": "ok", "ready": true,
        "generation": {
            "id": 3, "ageSeconds": 600, "degraded": false, "retired": 0, "retiredOldestAgeSeconds": 0,
        },
        "pools": pools, "overdueRequests": 0, "stuckRequests": 0,
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(body).expect("a status summary")
}

fn verdict(summary: &StatusSummary, limits: &Thresholds) -> (u8, String) {
    let verdict = summary.judge(limits);
    (verdict.exit_code(), plugin_line(summary, &verdict, limits))
}

#[test]
fn a_healthy_farm_is_ok_and_the_line_carries_performance_data() {
    let farm = summary(vec![pool("farm", 4, 4), pool("web", 2, 2)], json!({}));
    let (code, line) = verdict(&farm, &Thresholds::default());
    assert_eq!(code, 0);
    assert!(
        line.starts_with("OXIROUTE OK - 2 pools, 6/6 servers available | farm_available=4;4;1 ")
    );
    assert!(line.contains("web_available=2;2;1 ") && line.contains(" overdue=0 stuck=0 "));
    assert!(!line.contains('\n'));
}

#[test]
fn one_missing_server_warns_and_an_empty_pool_is_critical() {
    let (code, line) = verdict(
        &summary(vec![pool("farm", 3, 4)], json!({})),
        &Thresholds::default(),
    );
    assert_eq!(
        (code, line.contains("WARNING - pool farm 3/4 available")),
        (1, true)
    );
    let (code, line) = verdict(
        &summary(vec![pool("farm", 0, 4)], json!({})),
        &Thresholds::default(),
    );
    assert_eq!(
        (code, line.contains("CRITICAL - pool farm 0/4 available")),
        (2, true)
    );
}

#[test]
fn the_operator_thresholds_replace_the_defaults() {
    let farm = summary(vec![pool("farm", 2, 4)], json!({}));
    let limits = Thresholds {
        warn_available_below: Some(2),
        crit_available_below: 3,
        ..Thresholds::default()
    };
    assert_eq!(verdict(&farm, &limits).0, 2);
    let relaxed = Thresholds {
        warn_available_below: Some(2),
        ..Thresholds::default()
    };
    let (code, line) = verdict(&farm, &relaxed);
    assert_eq!(code, 0);
    assert!(line.contains("farm_available=2;2;1"));
}

#[test]
fn queue_wait_and_request_age_limits_warn() {
    let mut busy = pool("farm", 4, 4);
    busy["queued"] = json!(7);
    busy["oldestQueueWaitMs"] = json!(9_000);
    busy["oldestInflightMs"] = json!(91_000);
    let farm = summary(vec![busy], json!({}));
    assert_eq!(verdict(&farm, &Thresholds::default()).0, 1);
    let lenient = Thresholds {
        max_queue_wait_ms: 10_000,
        ..Thresholds::default()
    };
    assert_eq!(verdict(&farm, &lenient).0, 0);
    let strict = Thresholds {
        max_queue_wait_ms: 10_000,
        max_inflight_age_ms: Some(60_000),
        ..Thresholds::default()
    };
    let (code, line) = verdict(&farm, &strict);
    assert_eq!(
        (code, line.contains("oldest request is 91000 ms old")),
        (1, true)
    );
}

#[test]
fn overdue_requests_and_an_unready_process_are_critical_and_stuck_ones_warn() {
    let farm = |extra| summary(vec![pool("farm", 4, 4)], extra);
    let limits = Thresholds::default();
    assert_eq!(
        verdict(&farm(json!({ "overdueRequests": 2 })), &limits).0,
        2
    );
    assert_eq!(verdict(&farm(json!({ "ready": false })), &limits).0, 2);
    assert_eq!(verdict(&farm(json!({ "stuckRequests": 1 })), &limits).0, 1);
}

#[test]
fn a_refused_configuration_and_a_lingering_retired_generation_warn() {
    let mut generation = json!({
        "id": 3, "ageSeconds": 600, "degraded": true, "retired": 1, "retiredOldestAgeSeconds": 7_300,
        "lastRejection": { "atUnixMs": 1, "ageSeconds": 120 },
    });
    let farm = summary(
        vec![pool("farm", 4, 4)],
        json!({ "generation": generation.take() }),
    );
    let (code, line) = verdict(&farm, &Thresholds::default());
    assert_eq!(code, 1);
    assert!(line.contains("configuration refused 120 s ago"));
    assert!(line.contains("retired generation still draining after 7300 s"));
    let patient = Thresholds {
        max_retired_age_seconds: 10_800,
        ..Thresholds::default()
    };
    assert!(
        !verdict(&farm, &patient)
            .1
            .contains("retired generation still draining")
    );
}

#[test]
fn registry_problems_are_judged() {
    let registry = |body: Value| summary(vec![pool("farm", 4, 4)], json!({ "registry": body }));
    let clean =
        json!({ "degraded": false, "leases": 3, "lateLeases": 0, "shadowed": 0, "orphaned": 0 });
    assert_eq!(
        verdict(&registry(clean.clone()), &Thresholds::default()).0,
        0
    );
    let late =
        json!({ "degraded": false, "leases": 3, "lateLeases": 1, "shadowed": 0, "orphaned": 0 });
    let (code, line) = verdict(&registry(late), &Thresholds::default());
    assert_eq!(
        (
            code,
            line.contains("1 registry leases late for a heartbeat")
        ),
        (1, true)
    );
    let broken =
        json!({ "degraded": true, "leases": 3, "lateLeases": 0, "shadowed": 0, "orphaned": 0 });
    assert_eq!(verdict(&registry(broken), &Thresholds::default()).0, 2);
}

#[test]
fn the_worst_finding_leads_the_line() {
    let farm = summary(
        vec![pool("web", 1, 2), pool("farm", 0, 4)],
        json!({ "stuckRequests": 1 }),
    );
    let (code, line) = verdict(&farm, &Thresholds::default());
    assert_eq!(code, 2);
    assert!(line.starts_with("OXIROUTE CRITICAL - pool farm 0/4 available, "));
}
