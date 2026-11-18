use serde_json::{Value, json};

use crate::{cli::top::source::Sample, observability::stats_page::model::StatsSnapshot};

pub(super) struct Load {
    pub(super) requests: u64,
    pub(super) errors: u64,
    pub(super) slow: u64,
}

fn counter(value: u64) -> String {
    value.to_string()
}

fn histogram(load: &Load) -> Value {
    let all = load.requests + load.errors;
    let fast = all - load.slow;
    let buckets = [0, 0, fast, fast, fast, all, all, all].map(counter);
    json!({ "count": counter(all), "sumMs": counter(all * 8), "buckets": buckets })
}

fn traffic(load: &Load, current: u64) -> Value {
    json!({
        "sessions": { "current": current, "total": counter(load.requests + load.errors) },
        "bytesSent": counter(load.requests * 500),
        "bytesReceived": counter(load.requests * 4_000),
        "responses": {
            "1xx": "0", "2xx": counter(load.requests), "3xx": "0", "4xx": "3", "5xx": counter(load.errors), "none": "0"
        },
        "attempts": {
            "ok": counter(load.requests), "connectFailed": counter(load.errors), "timeout": "0",
            "protocolError": "0", "statusRetry": "0", "rescued": "0", "clientClosed": "0", "evacuated": "0"
        },
        "evacuated": { "redispatched": "0", "leftRunning": "0", "forced": "0", "failed": "0" },
    })
}

fn server(name: &str, status: &str, load: &Load) -> Value {
    json!({
        "name": name,
        "address": format!("192.0.2.{}:3000", name.len()),
        "origin": "static",
        "status": status,
        "available": status == "up",
        "administrativeState": "ready",
        "healthOverride": "auto",
        "health": "healthy",
        "weight": 1,
        "configuredWeight": 1,
        "maxConnections": 50,
        "check": {
            "enabled": true, "running": false, "successful": "9", "failed": "2",
            "consecutiveSuccesses": 9, "consecutiveFailures": 0
        },
        "passive": {
            "ejected": false, "failureCount": "0", "consecutiveFailures": 0, "ejectionCount": "0", "recoveryCount": "0"
        },
        "traffic": traffic(load, 2),
        "latency": { "request": histogram(load), "header": histogram(load) },
    })
}

fn pool(name: &str, servers: &[Value], load: &Load) -> Value {
    json!({
        "name": name,
        "algorithm": "round_robin",
        "status": "up",
        "availableServers": servers.len(),
        "totalServers": servers.len(),
        "unavailableSelections": "0",
        "failOpenSelections": "0",
        "queue": {
            "current": 1, "oldestWaitMs": 30, "total": "0", "timeouts": "0", "cancellations": "0",
            "wait": { "count": "0", "sumMs": "0", "buckets": ["0", "0"] }
        },
        "inflight": { "requests": 3, "oldestAgeMs": 10 },
        "traffic": traffic(load, 4),
        "latency": { "request": histogram(load), "header": histogram(load) },
        "servers": servers,
    })
}

pub(super) fn snapshot(sampled_at_unix_ms: u64, uptime_ms: u64, load: &Load) -> StatsSnapshot {
    let all = load.requests + load.errors;
    let quiet = Load {
        requests: load.requests / 2,
        errors: 0,
        slow: 0,
    };
    serde_json::from_value(json!({
        "schemaVersion": 1,
        "sampledAtUnixMs": sampled_at_unix_ms,
        "intervalHintMs": 1_000,
        "process": {
            "version": "7.1.0", "uptimeMs": uptime_ms, "state": "healthy", "administrativeState": "ready",
            "cpuPercent": 12.5, "residentMemoryBytes": 150_000_000, "threadCount": 8, "openFileDescriptors": 64,
            "activeConnections": 7, "maxConnections": 4_096, "acceptedConnections": counter(all),
            "rejectedConnections": "2", "bytesReceived": counter(load.requests * 4_000),
            "bytesSent": counter(load.requests * 500), "retryAttempts": "1",
            "inflight": { "upstreamRequests": 3, "oldestUpstreamAgeMs": 120, "overdue": 0, "stuck": 0 },
            "host": { "state": "healthy", "loadAverage1m": 0.5, "loadAverage5m": 0.4, "loadAverage15m": 0.3 },
        },
        "generation": {
            "number": 4, "ageMs": 600_000, "accepting": true, "degraded": false, "activeRevision": "rev-4",
            "prepares": "5", "activations": "4", "configRejections": "0", "failures": "0", "rollbacks": "0",
            "maxRetiredGenerations": 4, "retired": [],
        },
        "workers": {
            "scaling": "dynamic",
            "runtimes": [{ "runtime": "edge", "workers": 4, "active": 2, "relocations": "0" }],
        },
        "bucketBoundsMs": { "operation": [1, 5, 10, 50, 100, 500, 1_000, 5_000], "queueWait": [1, 5] },
        "listeners": [{
            "name": "edge", "protocol": "http", "bind": "socket:127.0.0.1:8080", "state": "listening",
            "administrativeState": "ready", "maxConnections": 4_096, "activeConnections": 5,
            "acceptedConnections": counter(all), "rejectedConnections": "0",
            "bytesReceived": counter(load.requests * 4_000), "bytesSent": counter(load.requests * 500),
            "http": {
                "outcomes": [
                    { "result": "success", "count": counter(load.requests) },
                    { "result": "server_error", "count": counter(load.errors) },
                ],
                "total": counter(all),
                "latency": histogram(load),
            },
            "cache": { "hits": counter(load.requests * 3), "misses": counter(load.requests), "admissions": "0", "evictions": "0" },
        }],
        "routes": [],
        "pools": [
            pool("web", &[server("origin-a", "up", load), server("origin-bb", "down", &quiet)], load),
            pool("api", &[server("origin-c", "drain", &quiet)], &quiet),
        ],
        "certificates": [],
        "certificateWatchers": [],
        "warnings": [],
    }))
    .expect("the fixture is a stats snapshot")
}

pub(super) fn sample(snapshot: StatsSnapshot) -> Sample {
    Sample {
        snapshot,
        requests: None,
    }
}

pub(super) fn with_requests(snapshot: StatsSnapshot) -> Sample {
    let requests = serde_json::from_value(json!({
        "total": "3",
        "truncated": true,
        "requests": [
            {
                "requestId": "req-old", "listener": "edge", "route": "site#0", "pool": "web", "server": "origin-a",
                "method": "POST", "host": "app.example.com", "path": "/v1/render", "clientIp": "192.0.2.50",
                "phase": "streaming", "startedAtUnixMs": 1_000, "ageMs": "42000", "attempts": 2, "overdue": true
            },
            {
                "requestId": "req-queued", "listener": "edge", "pool": "web", "method": "GET", "path": "/slow",
                "phase": "queued", "startedAtUnixMs": 2_000, "ageMs": "1500", "waitedMs": "1400",
                "queuePosition": 1, "attempts": 0, "overdue": false
            },
        ],
    }))
    .expect("the fixture is a request list");
    Sample {
        snapshot,
        requests: Some(requests),
    }
}
