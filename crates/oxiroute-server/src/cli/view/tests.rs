use serde_json::{Value, json};

use super::View;

fn endpoint(name: &str, administrative: &str, state: &str) -> Value {
    json!({
        "name": name, "address": "192.0.2.10:8080", "administrativeState": administrative,
        "state": state, "healthOverride": "auto", "weight": 3, "activeConnections": "12",
        "maxConnections": 100, "configuredMaxConnections": null, "origin": "static",
        "checksEnabled": true, "successfulChecks": "40", "failedChecks": "2",
        "consecutiveFailures": "0", "lastFailure": null, "passiveEjected": false,
        "passiveEjectionReason": null,
    })
}

fn pools() -> Value {
    json!({ "pools": [
        {
            "name": "web", "algorithm": "round_robin", "availableEndpoints": 1, "totalEndpoints": 2,
            "queued": 0, "queueTimeouts": "0",
            "endpoints": [endpoint("a", "ready", "healthy"), endpoint("b", "drain", "unhealthy")],
        },
        {
            "name": "db", "algorithm": "least_connections", "availableEndpoints": 1, "totalEndpoints": 1,
            "queued": 4, "queueTimeouts": "1500",
            "endpoints": [endpoint("a", "ready", "healthy")],
        },
    ] })
}

fn table(view: View, value: &Value) -> String {
    view.document(value).table().stdout
}

fn plain(view: View, value: &Value) -> String {
    view.document(value).plain()
}

#[test]
fn pool_list_shows_a_row_per_pool_with_the_state_of_its_servers() {
    assert_eq!(
        table(View::Pools, &pools()),
        "NAME  ALGORITHM          ADMIN  AVAILABLE  SERVERS  QUEUED  QUEUE TIMEOUTS\n\
         web   round_robin        mixed          1        2       0               0\n\
         db    least_connections  ready          1        1       4            1.5k\n"
    );
}

#[test]
fn pool_detail_lists_the_servers_under_the_pool() {
    let text = table(
        View::Pool,
        &json!({ "pools": [pools()["pools"][0].clone()] }),
    );
    assert_eq!(
        text,
        "name            web\nalgorithm       round_robin\nadmin           mixed\navailable       1\nservers         2\n\
         queued          0\nqueue timeouts  0\n\n\
         SERVER  ADDRESS          ADMIN  HEALTH     OVERRIDE  WEIGHT  ACTIVE  MAX  ORIGIN\n\
         a       192.0.2.10:8080  ready  healthy    auto           3      12  100  static\n\
         b       192.0.2.10:8080  drain  unhealthy  auto           3      12  100  static\n"
    );
}

#[test]
fn server_list_names_the_pool_and_hides_json_blobs() {
    let value =
        json!({ "servers": [{ "pool": "web", "server": endpoint("a", "ready", "healthy") }] });
    let text = table(View::Servers, &value);
    assert_eq!(
        text,
        "POOL  SERVER  ADDRESS          ADMIN  HEALTH   OVERRIDE  WEIGHT  ACTIVE  MAX  ORIGIN\n\
         web   a       192.0.2.10:8080  ready  healthy  auto           3      12  100  static\n"
    );
    assert!(!text.contains('{'));
}

#[test]
fn server_list_reads_the_flat_entries_of_a_registry() {
    let mut entry = endpoint("registered", "ready", "healthy");
    entry["pool"] = json!("web");
    let text = plain(View::Servers, &json!({ "servers": [entry] }));
    assert!(
        text.starts_with(
            "pool=web server=registered address=192.0.2.10:8080 admin=ready health=healthy"
        ),
        "{text}"
    );
}

#[test]
fn server_detail_adds_the_health_check_counters() {
    let value =
        json!({ "servers": [{ "pool": "web", "server": endpoint("a", "ready", "healthy") }] });
    let text = table(View::Server, &value);
    assert!(
        text.contains(
            "checks succeeded      40\nchecks failed         2\nconsecutive failures  0\n"
        ),
        "{text}"
    );
    assert!(text.contains("last failure          -\n"), "{text}");
}

#[test]
fn listeners_show_traffic_with_units_and_plain_keeps_raw_numbers() {
    let value = json!({ "listeners": [{
        "name": "edge", "protocol": "http", "bind": "socket:192.0.2.1:443", "administrativeState": "ready",
        "state": "listening", "activeConnections": 7, "maxConnections": null, "acceptedConnections": "2500",
        "rejectedConnections": "0", "bytesReceived": "1536", "bytesSent": "3145728",
    }] });
    assert_eq!(
        table(View::Listeners, &value),
        "NAME  PROTOCOL  BIND           ADMIN  STATE      ACTIVE  MAX  ACCEPTED  REJECTED  RECEIVED  SENT\n\
         edge  http      192.0.2.1:443  ready  listening       7    -      2.5k         0      1.5K  3.0M\n"
    );
    assert_eq!(
        plain(View::Listeners, &value),
        "name=edge protocol=http bind=192.0.2.1:443 admin=ready state=listening active=7 accepted=2500 rejected=0 \
         received_bytes=1536 sent_bytes=3145728\n"
    );
}

#[test]
fn generation_status_is_a_record_with_short_revisions() {
    let value = json!({ "generation": {
        "buildVersion": "7.1.0", "generation": 3, "activeRevision": "a".repeat(64), "previousRevision": null,
        "candidateRevision": null, "diskRevision": "b".repeat(64), "quarantinedRevision": null,
        "activeAccepting": true, "degraded": true, "lastFailure": null,
        "lastRejection": { "atUnixMs": 1, "revision": "c", "diagnostics": [
            { "code": "E_INVALID_FIELD", "stage": "validate", "message": "listener bind is taken" },
            { "code": "E_OTHER", "stage": "validate", "message": "second" },
        ] },
        "prepares": 4, "activations": 3, "configRejections": 1, "failures": 0, "rollbacks": 0,
        "retiredGenerations": [{}, {}], "pendingApply": { "revision": "d", "reason": "listener_pending" },
    } });
    let text = table(View::Generation, &value);
    assert!(text.contains("\nactive          aaaaaaaaaaaa\n"), "{text}");
    assert!(text.contains("\ndegraded        yes\n"), "{text}");
    assert!(
        text.contains("\nlast rejection  E_INVALID_FIELD: listener bind is taken (+1 more)\n"),
        "{text}"
    );
    assert!(
        text.contains("\npending apply   listener_pending\n"),
        "{text}"
    );
    assert!(text.ends_with("retired         2\n"), "{text}");
}

#[test]
fn a_reload_waiting_for_a_draining_worker_shows_its_connections() {
    let value = json!({ "generation": { "pendingApply": {
        "revision": "d", "reason": "retired_draining", "connections": 3,
    } } });

    let text = table(View::Generation, &value);

    assert!(
        text.contains("\npending apply   retired_draining (3 connections)\n"),
        "{text}"
    );
}

#[test]
fn status_leads_with_build_state_and_revision_then_lists_listeners() {
    let value = json!({
        "buildVersion": "7.1.0", "buildId": "gnu:ab", "masterBuildId": "gnu:cd", "degraded": false,
        "activeRevision": "f".repeat(64), "previousRevision": null,
        "candidateRevision": null, "diskRevision": "e".repeat(64), "activeGenerationAgeMs": 3_725_000,
        "components": { "process": { "state": "healthy" }, "host": { "state": "healthy" },
            "generation": { "state": "healthy" }, "audit": { "state": "degraded" } },
        "capabilities": { "supervision": { "mode": "supervised" } },
        "listeners": [{ "name": "edge", "protocol": "http", "bind": "socket:192.0.2.1:80",
            "administrativeState": "ready", "state": "listening" }],
    });
    let text = table(View::Status, &value);
    assert!(
        text.starts_with(
            "build            7.1.0\nbuild id         gnu:ab\nmaster build id  gnu:cd\nstate            healthy\nactive           ffffffffffff\n"
        ),
        "{text}"
    );
    assert!(text.contains("generation age   1h02m\n"), "{text}");
    assert!(
        text.contains("audit            degraded\n\nNAME  PROTOCOL"),
        "{text}"
    );
    assert!(plain(View::Status, &value).starts_with(
        "record=status build=7.1.0 build_id=gnu:ab master_build_id=gnu:cd state=healthy active=ffff"
    ));
}

#[test]
fn status_names_the_master_build_only_under_a_supervising_master() {
    let direct = json!({ "buildVersion": "7.1.0", "buildId": "gnu:ab", "masterBuildId": null });
    let text = table(View::Status, &direct);
    assert!(
        text.starts_with("build           7.1.0\nbuild id        gnu:ab\n"),
        "{text}"
    );
    assert!(!text.contains("master"), "{text}");
    assert!(!plain(View::Status, &direct).contains("master_build_id"));
}

#[test]
fn a_required_restart_is_the_first_row_of_status_and_generation_status() {
    let restart = json!({ "required": true, "candidateRevision": "b".repeat(64), "since": 1,
        "reasons": [
            { "code": "unix_listener_mode", "path": "/config/listeners/live/bind/mode", "summary": "kept 0600" },
            { "code": "registry_settings", "path": "/config/registry", "summary": "kept" },
        ] });
    let banner = "RESTART REQUIRED: unix_listener_mode: kept 0600 (+1 more)";
    let generation =
        json!({ "generation": { "buildVersion": "7.1.0", "restartRequired": restart } });
    let status = json!({ "buildVersion": "7.1.0", "restartRequired": restart, "listeners": [] });

    for (view, value) in [(View::Generation, &generation), (View::Status, &status)] {
        assert!(
            table(view, value).starts_with(&format!("restart required  {banner}\nbuild")),
            "{}",
            table(view, value)
        );
        assert!(plain(view, value).contains(&format!("restart_required=\"{banner}\"")));
    }
    let quiet =
        json!({ "generation": { "restartRequired": { "required": false, "reasons": [] } } });
    assert!(!table(View::Generation, &quiet).contains("restart"));
}

#[test]
fn events_print_utc_times_and_point_at_the_next_page() {
    let value = json!({ "cursor": 7, "hasMore": true, "events": [{
        "cursor": 6, "timestampUnixMs": 1_795_018_962_349_u64, "event": "generation_activate", "outcome": "activated",
        "revision": "a".repeat(64), "correlationId": "op-1", "actor": "system",
    }] });
    let document = View::Events.document(&value);
    let text = document.table();
    assert_eq!(
        text.stdout,
        "CURSOR  TIME                  EVENT                OUTCOME    ACTOR   REVISION      CORRELATION\n\
         \x20    6  2026-11-18T16:22:42Z  generation_activate  activated  system  aaaaaaaaaaaa  op-1\n"
    );
    assert_eq!(
        text.notices,
        ["more events are available; continue with --after 7"]
    );
}

#[test]
fn requests_show_age_and_warn_when_truncated() {
    let value = json!({ "total": "250", "truncated": true, "requests": [{
        "requestId": "r-1", "listener": "edge", "route": null, "pool": "web", "server": "a", "method": "GET",
        "host": "app.example.com", "path": "/v1/render", "phase": "upstream", "startedAtUnixMs": 1, "ageMs": "65000",
        "attempts": 1, "overdue": false,
    }] });
    let text = View::Requests.document(&value).table();
    assert_eq!(
        text.stdout,
        "ID     AGE  PHASE     POOL  SERVER  METHOD  HOST             PATH        ATTEMPTS\n\
         r-1  1m05s  upstream  web   a       GET     app.example.com  /v1/render         1\n"
    );
    assert_eq!(
        text.notices,
        ["showing 1 of 250 requests; raise --limit to see more"]
    );
}

#[test]
fn rtmp_streams_show_publisher_media_and_age() {
    let value = json!({ "as_of_unix_ms": 10_000, "streams": [{
        "id": "s-1", "application": "live", "name": "cam", "created_at_unix_ms": 4_000,
        "publisher": { "session_id": "p-1" }, "subscriber_count": 2,
        "media": { "audio": { "payload_bytes": "2048" }, "video": { "payload_bytes": "5242880" } },
    }] });
    assert_eq!(
        plain(View::RtmpStreams, &value),
        "id=s-1 application=live name=cam publisher=p-1 subscribers=2 audio_bytes=2048 video_bytes=5242880 age_ms=6000\n"
    );
}

#[test]
fn unknown_shapes_flatten_to_labelled_fields_instead_of_json() {
    let value = json!({
        "outcome": "startup_requested", "candidateRevision": "abc",
        "nested": { "limitMs": 5, "tags": ["a", "b"] }, "gone": null,
        "outcomes": [{ "name": "x", "error": { "code": "e1" } }, { "name": "y" }],
    });
    let document = View::Generic.document(&value);
    assert_eq!(
        document.plain(),
        "record=summary candidate_revision=abc nested.limit_ms=5 nested.tags=a,b outcome=startup_requested\n\
         record=outcomes error.code=e1 name=x\nrecord=outcomes name=y\n"
    );
    assert_eq!(
        document.table().stdout,
        "candidate revision  abc\nnested.limit ms     5\nnested.tags         a,b\noutcome             startup_requested\n\n\
         ERROR.CODE  NAME\ne1          x\n-           y\n"
    );
}

#[test]
fn empty_collections_name_what_is_missing() {
    assert_eq!(
        View::Tls
            .document(&json!({ "certificates": [], "watcher": null }))
            .table()
            .notices,
        ["no certificates"]
    );
    assert_eq!(
        View::Pools
            .document(&json!({ "pools": [] }))
            .table()
            .notices,
        ["no pools"]
    );
    assert_eq!(
        View::Servers
            .document(&json!({ "servers": [] }))
            .table()
            .notices,
        ["no servers"]
    );
    assert_eq!(
        View::Listeners
            .document(&json!({ "listeners": [] }))
            .table()
            .notices,
        ["no listeners"]
    );
    assert_eq!(
        View::Events
            .document(&json!({ "events": [] }))
            .table()
            .notices,
        ["no events"]
    );
    assert_eq!(
        View::Requests
            .document(&json!({ "requests": [] }))
            .table()
            .notices,
        ["no requests"]
    );
    assert_eq!(
        View::RtmpStreams
            .document(&json!({ "streams": [] }))
            .table()
            .notices,
        ["no streams"]
    );
}

#[test]
fn mutation_responses_show_where_the_change_lives() {
    let applied = json!({ "outcome": "applied", "changed": 2, "persistence": "mixed" });
    assert_eq!(
        plain(View::Generic, &applied),
        "changed=2 outcome=applied persistence=mixed\n"
    );
    let evacuated = json!({
        "outcome": "evacuated", "persistence": "registry", "force": false,
        "cancelledAndRedispatched": 1, "forced": 0, "leftRunning": {},
    });
    assert!(plain(View::Evacuation, &evacuated).contains("persistence=registry"));
    let refreshed =
        json!({ "outcome": "refreshed", "persistence": "runtime", "atomic": false, "servers": [] });
    assert!(plain(View::DnsRefresh, &refreshed).contains("persistence=runtime"));
}
