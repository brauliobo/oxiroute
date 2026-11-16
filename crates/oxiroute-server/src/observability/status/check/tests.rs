use serde_json::{Value, json};

use super::*;

fn pool(name: &str, available: u64, total: u64) -> Value {
    json!({
        "name": name, "state": "ok", "available": available, "total": total, "ejected": 0,
        "failOpen": false, "queued": 0, "oldestQueueWaitMs": 0, "inflight": 0, "oldestInflightMs": 0,
    })
}

fn summary(pools: &[Value], extra: &Value) -> StatusSummary {
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
    let farm = summary(&[pool("farm", 4, 4), pool("web", 2, 2)], &json!({}));
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
        &summary(&[pool("farm", 3, 4)], &json!({})),
        &Thresholds::default(),
    );
    assert_eq!(
        (code, line.contains("WARNING - pool farm 3/4 available")),
        (1, true)
    );
    let (code, line) = verdict(
        &summary(&[pool("farm", 0, 4)], &json!({})),
        &Thresholds::default(),
    );
    assert_eq!(
        (code, line.contains("CRITICAL - pool farm 0/4 available")),
        (2, true)
    );
}

#[test]
fn the_operator_thresholds_replace_the_defaults() {
    let farm = summary(&[pool("farm", 2, 4)], &json!({}));
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
    let farm = summary(&[busy], &json!({}));
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
    let farm = |extra| summary(&[pool("farm", 4, 4)], &extra);
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
    let generation = json!({
        "id": 3, "ageSeconds": 600, "degraded": true, "retired": 1, "retiredOldestAgeSeconds": 7_300,
        "lastRejection": { "atUnixMs": 1, "ageSeconds": 120 },
    });
    let farm = summary(&[pool("farm", 4, 4)], &json!({ "generation": generation }));
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
    let registry = |body: Value| summary(&[pool("farm", 4, 4)], &json!({ "registry": body }));
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
        &[pool("web", 1, 2), pool("farm", 0, 4)],
        &json!({ "stuckRequests": 1 }),
    );
    let (code, line) = verdict(&farm, &Thresholds::default());
    assert_eq!(code, 2);
    assert!(line.starts_with("OXIROUTE CRITICAL - pool farm 0/4 available, "));
}

fn canary_pool(canary: &Value) -> Value {
    let mut farm = pool("farm", 4, 4);
    farm["canary"] = canary.clone();
    farm
}

fn canary(result: &str, failures: u64, attempt_age: u64) -> Value {
    json!({
        "result": result, "reason": "unexpected_status", "server": "a", "durationMs": 840,
        "lastAttemptAgeSeconds": attempt_age, "lastSuccessAgeSeconds": 90,
        "consecutiveFailures": failures, "intervalMs": 30_000,
    })
}

#[test]
fn a_healthy_canary_adds_performance_data_but_no_finding() {
    let farm = summary(&[canary_pool(&canary("ok", 0, 10))], &json!({}));
    let (code, line) = verdict(&farm, &Thresholds::default());
    assert_eq!(code, 0);
    assert!(line.contains(" farm_canary_ms=840 "), "{line}");
}

#[test]
fn a_failing_canary_warns_once_and_turns_critical_when_it_repeats() {
    let once = summary(&[canary_pool(&canary("failed", 1, 10))], &json!({}));
    let (code, line) = verdict(&once, &Thresholds::default());
    assert_eq!(code, 1);
    assert!(
        line.contains("pool farm canary failed (unexpected_status)"),
        "{line}"
    );
    let repeated = summary(&[canary_pool(&canary("failed", 2, 10))], &json!({}));
    assert_eq!(verdict(&repeated, &Thresholds::default()).0, 2);
}

#[test]
fn a_slow_canary_warns_and_one_that_stopped_running_is_reported() {
    let slow = summary(&[canary_pool(&canary("slow", 0, 10))], &json!({}));
    let (code, line) = verdict(&slow, &Thresholds::default());
    assert_eq!(
        (code, line.contains("canary answered slowly (840 ms)")),
        (1, true)
    );
    let stalled = summary(&[canary_pool(&canary("ok", 0, 100))], &json!({}));
    let (code, line) = verdict(&stalled, &Thresholds::default());
    assert_eq!(
        (code, line.contains("canary has not run for 100 s")),
        (1, true)
    );
}

#[test]
fn busy_unavailable_and_pending_canaries_say_nothing_extra() {
    for state in [
        json!({ "result": "busy", "lastAttemptAgeSeconds": 5, "consecutiveFailures": 0, "intervalMs": 30_000, "durationMs": 0 }),
        json!({ "result": "unavailable", "lastAttemptAgeSeconds": 5, "consecutiveFailures": 0, "intervalMs": 30_000, "durationMs": 0 }),
        json!({ "result": null, "consecutiveFailures": 0, "intervalMs": 30_000, "durationMs": 0 }),
    ] {
        let farm = summary(&[canary_pool(&state)], &json!({}));
        assert_eq!(verdict(&farm, &Thresholds::default()).0, 0, "{state}");
    }
}
