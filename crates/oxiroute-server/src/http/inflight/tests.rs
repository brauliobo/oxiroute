use super::*;
use crate::http::evacuation::{Cancellation, LeftRunning};

fn arrival(request_id: &str, deadline: Duration) -> InflightArrival<'_> {
    InflightArrival {
        request_id,
        listener: "edge".into(),
        method: Method::POST,
        host: Some("app.test"),
        path: "/v1/render",
        client_ip: Some("192.0.2.9".parse().expect("test address")),
        started: Instant::now(),
        deadline,
        access_log: None,
    }
}

fn registry_for_test() -> &'static InflightRegistry {
    Box::leak(Box::new(InflightRegistry::new()))
}

#[test]
fn request_texts_are_split_back_and_bounded_each() {
    let texts = Texts::new("abc", None, "/x?y=1");
    assert_eq!(
        (texts.request_id(), texts.host(), texts.path()),
        ("abc", None, "/x")
    );
    let long = "\u{e9}".repeat(200);
    let texts = Texts::new(&long, Some(&long), &format!("/{long}"));
    for text in [
        texts.request_id(),
        texts.host().expect("host"),
        texts.path(),
    ] {
        assert!(text.len() <= MAX_PATH_BYTES && long.contains(text.trim_start_matches('/')));
    }
    assert_eq!(texts.request_id(), texts.host().expect("host"));
}

#[test]
fn an_entry_lives_exactly_as_long_as_its_guard() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("a", Duration::from_secs(30)));
    guard.set_pool("farm");
    assert_eq!(registry.list(&InflightFilter::default(), 10).total, 1);
    assert_eq!(
        registry.find("a").expect("entry").pool.as_deref(),
        Some("farm")
    );
    drop(guard);
    assert_eq!(registry.list(&InflightFilter::default(), 10).total, 0);
    assert!(registry.find("a").is_none());
}

#[test]
fn many_mixed_requests_leave_no_entries_behind() {
    let registry = registry_for_test();
    let mut guards = Vec::new();
    for index in 0..10_000 {
        let guard = registry.register(arrival(&format!("r{index}"), Duration::from_secs(1)));
        if index % 3 == 0 {
            guards.push(guard);
        }
    }
    assert_eq!(
        registry.list(&InflightFilter::default(), 1).total,
        guards.len()
    );
    drop(guards);
    assert_eq!(registry.list(&InflightFilter::default(), 1).total, 0);
}

#[test]
fn phases_pool_server_and_attempts_follow_the_request() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("p", Duration::from_secs(30)));
    for phase in [Phase::Queued, Phase::Connecting, Phase::AwaitingHeaders] {
        guard.set_phase(phase, Instant::now());
        assert_eq!(registry.find("p").expect("entry").phase, phase);
    }
    guard.set_server("gpu-1");
    guard.set_server("gpu-2");
    let view = registry.find("p").expect("entry");
    assert_eq!((view.server.as_deref(), view.attempts), (Some("gpu-2"), 2));
}

#[test]
fn an_entry_that_served_a_request_starts_the_next_one_as_a_new_arrival() {
    let registry = registry_for_test();
    let first = attempt_on(
        registry,
        "first",
        "farm",
        "gpu-1",
        Some(LeftRunning::DataPlane),
    );
    first.set_phase(Phase::Streaming, Instant::now());
    first.place_route("api".into());
    let held_cell = first.queue_cell();
    held_cell.store(3, Ordering::Relaxed);
    drop(first);

    let second = registry.register(arrival("second", Duration::from_secs(30)));
    let view = registry.find("second").expect("entry");
    assert_eq!(
        (
            view.phase,
            view.attempts,
            view.pool,
            view.server,
            view.route
        ),
        (Phase::Routed, 0, None, None, None)
    );
    assert_eq!((view.waited_ms, view.queue_position), (None, None));
    let cell = second.queue_cell();
    assert!(!Arc::ptr_eq(&cell, &held_cell) && cell.load(Ordering::Relaxed) == 0);
    assert_eq!(second.evacuation().evacuate(true, true), None);
    assert!(!second.stuck());
    assert!(registry.find("first").is_none());
}

#[test]
fn an_entry_a_scan_still_holds_is_never_handed_to_another_request() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("held", Duration::from_secs(30)));
    let scanned = registry.entries();
    drop(guard);

    let next = registry.register(arrival("next", Duration::from_secs(30)));
    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].view().request_id, "held");
    assert_eq!(registry.find("next").expect("entry").request_id, "next");
    assert!(registry.find("held").is_none());
    drop(next);
}

#[test]
fn a_request_registered_with_its_route_and_pool_is_placed_on_arrival() {
    let registry = registry_for_test();
    let pool = Label::new("farm");
    let guard = registry.register_placed(
        arrival("placed", Duration::from_secs(30)),
        "route-1".into(),
        pool.shared(),
    );
    let view = registry.find("placed").expect("entry");
    assert_eq!(
        (
            view.route.as_deref(),
            view.pool.as_deref(),
            view.server,
            view.attempts
        ),
        (Some("route-1"), Some("farm"), None, 0)
    );
    guard.place_attempt(&pool, "gpu-1".into());
    guard.place_attempt(&Label::new("other"), "gpu-2".into());
    let view = registry.find("placed").expect("entry");
    assert_eq!(
        (view.pool.as_deref(), view.server.as_deref(), view.attempts),
        (Some("other"), Some("gpu-2"), 2)
    );
}

#[test]
fn filters_select_by_pool_phase_and_age_oldest_first() {
    let registry = registry_for_test();
    let first = registry.register(arrival("old", Duration::from_secs(30)));
    first.set_pool("farm");
    std::thread::sleep(Duration::from_millis(20));
    let second = registry.register(arrival("new", Duration::from_secs(30)));
    second.set_pool("farm");
    second.set_phase(Phase::Queued, Instant::now());
    let other = registry.register(arrival("other", Duration::from_secs(30)));
    other.set_pool("web");
    let filter = InflightFilter {
        pool: Some("farm".into()),
        ..InflightFilter::default()
    };
    let listing = registry.list(&filter, 10);
    let ids = listing
        .requests
        .iter()
        .map(|view| view.request_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["old", "new"]);
    let queued = InflightFilter {
        phase: Some(Phase::Queued),
        ..InflightFilter::default()
    };
    assert_eq!(registry.list(&queued, 10).requests[0].request_id, "new");
    let aged = InflightFilter {
        min_age_ms: 10,
        ..InflightFilter::default()
    };
    assert_eq!(registry.list(&aged, 10).total, 1);
    assert_eq!(registry.list(&filter, 1).total, 2);
    assert_eq!(registry.list(&filter, 1).requests.len(), 1);
}

#[test]
fn a_request_past_deadline_plus_grace_is_overdue() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("late", Duration::ZERO));
    assert_eq!(registry.totals().overdue, 0);
    std::thread::sleep(OVERDUE_GRACE + Duration::from_millis(50));
    assert_eq!(registry.totals().overdue, 1);
    guard.set_deadline(Duration::from_mins(1));
    assert_eq!(registry.totals().overdue, 0);
}

#[test]
fn a_silent_request_is_reported_stuck_exactly_once() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("slow", Duration::from_secs(30)));
    guard.set_pool("farm");
    guard.set_phase(Phase::AwaitingHeaders, Instant::now());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(registry.sweep(Duration::from_millis(10)).0.len(), 1);
    assert!(registry.sweep(Duration::from_millis(10)).0.is_empty());
    assert!(guard.stuck());
    let totals = registry.totals();
    assert_eq!(
        totals.stuck.get(&("farm".to_owned(), "awaiting_headers")),
        Some(&1)
    );
}

#[test]
fn a_streaming_request_that_keeps_moving_is_not_stuck() {
    let registry = registry_for_test();
    let guard = registry.register(arrival("stream", Duration::from_secs(30)));
    guard.set_phase(Phase::Streaming, Instant::now());
    std::thread::sleep(Duration::from_millis(30));
    guard.progress(Instant::now());
    assert!(registry.sweep(Duration::from_millis(20)).0.is_empty());
}

#[test]
fn the_watchdog_has_nothing_to_wait_for_until_a_request_can_go_quiet() {
    let registry = registry_for_test();
    let limit = Duration::from_mins(1);
    assert!(registry.sweep(limit).1.is_none());
    let guard = registry.register(arrival("fresh", Duration::from_secs(30)));
    let due = registry
        .sweep(limit)
        .1
        .expect("a request that can go quiet");
    assert!(due > Duration::from_secs(59) && due <= limit + Duration::from_millis(1));
    drop(guard);
    assert!(registry.sweep(limit).1.is_none());
}

fn attempt_on(
    registry: &'static InflightRegistry,
    id: &str,
    pool: &str,
    server: &str,
    unmovable: Option<LeftRunning>,
) -> InflightGuard {
    let guard = registry.register(arrival(id, Duration::from_secs(30)));
    guard.set_pool(pool);
    guard.set_server(server);
    guard.evacuation().begin_attempt(unmovable);
    guard
}

#[test]
fn evacuation_reaches_the_attempts_on_one_pool_and_server_of_any_generation() {
    let registry = registry_for_test();
    let movable = [
        attempt_on(registry, "old-generation", "farm", "node-a", None),
        attempt_on(registry, "new-generation", "farm", "node-a", None),
    ];
    let pinned = attempt_on(
        registry,
        "post",
        "farm",
        "node-a",
        Some(LeftRunning::NotReplayable),
    );
    let elsewhere = [
        attempt_on(registry, "other-server", "farm", "node-b", None),
        attempt_on(registry, "other-pool", "web", "node-a", None),
    ];
    let queued = registry.register(arrival("queued", Duration::from_secs(30)));
    queued.set_pool("farm");
    let retrying = attempt_on(registry, "retrying", "farm", "node-a", None);
    retrying.evacuation().settle();

    let report = registry.evacuate_server("farm", "node-a", false, true);

    assert_eq!(report.redispatched, 2);
    assert_eq!(report.forced, 0);
    assert_eq!(
        report.left_running.get(&LeftRunning::NotReplayable),
        Some(&1)
    );
    assert_eq!(report.left_running_total(), 1);
    assert!(
        movable
            .iter()
            .all(|guard| guard.evacuation().settle() == Some(Cancellation::Move))
    );
    assert_eq!(pinned.evacuation().settle(), None);
    assert!(
        elsewhere
            .iter()
            .all(|guard| guard.evacuation().settle().is_none())
    );
    assert_eq!(
        registry
            .evacuate_server("farm", "node-a", false, true)
            .redispatched,
        0
    );
}

#[test]
fn a_forced_evacuation_kills_what_cannot_move_and_still_moves_the_rest() {
    let registry = registry_for_test();
    let streaming = attempt_on(
        registry,
        "stream",
        "farm",
        "node-a",
        Some(LeftRunning::ResponseStarted),
    );
    let movable = attempt_on(registry, "movable", "farm", "node-a", None);

    let report = registry.evacuate_server("farm", "node-a", true, true);

    assert_eq!(
        (
            report.redispatched,
            report.forced,
            report.left_running_total()
        ),
        (1, 1, 0)
    );
    assert_eq!(streaming.evacuation().settle(), Some(Cancellation::Kill));
    assert_eq!(movable.evacuation().settle(), Some(Cancellation::Move));
}

#[test]
fn without_a_destination_movable_requests_stay_unless_forced() {
    let registry = registry_for_test();
    let movable = attempt_on(registry, "stuck-here", "farm", "node-a", None);

    let report = registry.evacuate_server("farm", "node-a", false, false);

    assert_eq!(
        report.left_running.get(&LeftRunning::NoDestination),
        Some(&1)
    );
    assert_eq!(movable.evacuation().settle(), None);
    let movable = attempt_on(registry, "cut-here", "farm", "node-a", None);

    let report = registry.evacuate_server("farm", "node-a", true, false);

    assert_eq!((report.forced, report.left_running_total()), (1, 0));
    assert_eq!(movable.evacuation().settle(), Some(Cancellation::Kill));
}

#[test]
fn paths_drop_the_query_and_are_cut_on_a_character_boundary() {
    assert_eq!(bounded_path("/a/b?token=secret"), "/a/b");
    let long = format!("/{}", "é".repeat(100));
    let bounded = bounded_path(&long);
    assert!(bounded.len() <= MAX_PATH_BYTES);
    assert!(long.starts_with(bounded));
}

#[test]
fn a_stuck_request_writes_one_arrival_line_to_the_access_log() {
    use oxiroute_config::AccessLogPolicy;

    let directory = tempfile::tempdir().expect("log directory");
    let path = directory.path().join("access.jsonl");
    let log = AccessLog::open("svc", Some(&AccessLogPolicy::File { path: path.clone() }))
        .expect("open access log")
        .expect("access log enabled");
    let registry = registry_for_test();
    let mut stuck = arrival("lost-1", Duration::from_secs(30));
    stuck.access_log = Some(Arc::new(log));
    let guard = registry.register(stuck);
    guard.set_pool("farm");
    guard.set_phase(Phase::AwaitingHeaders, Instant::now());
    std::thread::sleep(Duration::from_millis(30));
    registry.sweep(Duration::from_millis(10));
    registry.sweep(Duration::from_millis(10));
    let mut lines = Vec::new();
    for _ in 0..100 {
        lines = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        if !lines.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line: serde_json::Value = serde_json::from_str(&lines[0]).expect("stuck line");
    assert_eq!(line["event"], "stuck");
    assert_eq!(line["requestId"], "lost-1");
    assert_eq!(line["phase"], "awaiting_headers");
    assert_eq!(line["pool"], "farm");
    assert!(line["ageMs"].is_string());
}
