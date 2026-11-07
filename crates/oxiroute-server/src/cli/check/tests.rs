use super::*;

fn args(extra: &[&str]) -> CheckArgs {
    let mut words = vec!["oxiroute", "check"];
    words.extend_from_slice(extra);
    match Cli::try_parse_from(words).expect("check arguments").command {
        Some(Command::Check(args)) => args,
        _ => panic!("check command"),
    }
}

fn page(pool: &Value) -> Value {
    json!({
        "state": "ok", "ready": true,
        "generation": {
            "id": 1, "ageSeconds": 5, "degraded": false, "retired": 0, "retiredOldestAgeSeconds": 0,
        },
        "pools": [pool], "overdueRequests": 0, "stuckRequests": 0,
    })
}

fn farm(available: u64) -> Value {
    json!({
        "name": "farm", "state": "ok", "available": available, "total": 4, "ejected": 0,
        "failOpen": false, "queued": 0, "oldestQueueWaitMs": 0, "inflight": 0, "oldestInflightMs": 0,
    })
}

#[test]
fn defaults_follow_the_documented_thresholds() {
    let report = assess(page(&farm(3)), &args(&[]), None).expect("report");
    assert_eq!(report.code, 1);
    assert!(
        report
            .line
            .starts_with("OXIROUTE WARNING - pool farm 3/4 available | farm_available=3;4;1 ")
    );
    assert_eq!(report.json["status"], "warning");
    assert_eq!(report.json["exitCode"], 1);
}

#[test]
fn flags_set_the_thresholds_and_select_pools() {
    let strict = args(&["--warn-available-below", "2", "--crit-available-below", "3"]);
    assert_eq!(
        assess(page(&farm(2)), &strict, None).expect("report").code,
        2
    );
    let relaxed = args(&["--warn-available-below", "2"]);
    assert_eq!(
        assess(page(&farm(2)), &relaxed, None).expect("report").code,
        0
    );
    let other = args(&["--pool", "gpu"]);
    assert_eq!(
        assess(page(&farm(0)), &other, None).expect_err("a missing pool is unknown"),
        "pool gpu does not exist"
    );
    let only = args(&["--pool", "farm"]);
    assert_eq!(assess(page(&farm(0)), &only, None).expect("report").code, 2);
}

#[test]
fn a_page_that_is_not_a_status_document_is_not_understood() {
    let error = assess(json!({ "pools": 3 }), &args(&[]), None).expect_err("not a document");
    assert!(error.starts_with("the status document is not understood"));
}

#[test]
fn a_failed_client_canary_is_critical_and_a_slow_one_warns() {
    let failed = Finding {
        severity: Severity::Critical,
        pool: None,
        message: "canary http://host/ answered 502 instead of 200".into(),
    };
    let report = assess(page(&farm(4)), &args(&[]), Some(failed)).expect("report");
    assert_eq!(report.code, 2);
    assert!(
        report
            .line
            .contains("canary http://host/ answered 502 instead of 200")
    );
    let healthy = Finding {
        severity: Severity::Ok,
        pool: None,
        message: "canary answered".into(),
    };
    assert_eq!(
        assess(page(&farm(4)), &args(&[]), Some(healthy))
            .expect("report")
            .code,
        0
    );
}

#[test]
fn unknown_reports_use_exit_code_three_and_a_plugin_line() {
    let report = Report::unknown("status page unavailable: endpoint connection failed");
    assert_eq!(report.code, 3);
    assert_eq!(
        report.line,
        "OXIROUTE UNKNOWN - status page unavailable: endpoint connection failed"
    );
    assert_eq!(report.json["status"], "unknown");
}

#[test]
fn canary_options_need_each_other_and_usage_errors_are_recognised_as_check() {
    assert!(Cli::try_parse_from(["oxiroute", "check", "--canary"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "check", "--url", "http://h/"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "check", "--canary", "--url", "http://h/"]).is_ok());
    let words = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(is_invocation(&words(&["oxiroute", "check", "--bogus"])));
    assert!(is_invocation(&words(&[
        "oxiroute", "--output", "json", "check", "--pool"
    ])));
    assert!(!is_invocation(&words(&[
        "oxiroute", "server", "list", "--bogus"
    ])));
}
