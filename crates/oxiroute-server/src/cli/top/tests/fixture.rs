use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{
    cli::top::source::{Feed, Sample},
    observability::stats_page::model::StatsSnapshot,
};

pub(super) struct Load {
    pub(super) requests: u64,
    pub(super) errors: u64,
    pub(super) slow: u64,
}

pub(super) const fn load(requests: u64, errors: u64, slow: u64) -> Load {
    Load {
        requests,
        errors,
        slow,
    }
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

pub(super) fn snapshot_document(sampled_at_unix_ms: u64, uptime_ms: u64, load: &Load) -> Value {
    let all = load.requests + load.errors;
    let quiet = Load {
        requests: load.requests / 2,
        errors: 0,
        slow: 0,
    };
    json!({
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
    })
}

pub(super) fn snapshot(sampled_at_unix_ms: u64, uptime_ms: u64, load: &Load) -> StatsSnapshot {
    serde_json::from_value(snapshot_document(sampled_at_unix_ms, uptime_ms, load))
        .expect("the fixture is a stats snapshot")
}

pub(super) fn sample(snapshot: StatsSnapshot) -> Sample {
    Sample {
        snapshot,
        requests: Feed::Off,
        clients: Feed::Off,
        conns: Feed::Off,
        rtmp: Feed::Off,
    }
}

pub(super) fn requests_document(requests: &[(&str, u64)]) -> Value {
    let items = requests
        .iter()
        .map(|(id, age)| {
            json!({
                "requestId": id, "listener": "edge", "route": "site#0", "pool": "web", "server": "origin-a",
                "method": "GET", "host": "app.example.com", "path": format!("/{id}"), "clientIp": "192.0.2.50",
                "phase": "streaming", "startedAtUnixMs": 1_000, "ageMs": counter(*age), "attempts": 1,
                "overdue": *age > 30_000
            })
        })
        .collect::<Vec<_>>();
    json!({ "total": counter(items.len() as u64), "truncated": false, "requests": items })
}

pub(super) fn with_requests(snapshot: StatsSnapshot, requests: &[(&str, u64)]) -> Sample {
    Sample {
        requests: Feed::Ready(
            serde_json::from_value(requests_document(requests)).expect("a request list"),
        ),
        ..sample(snapshot)
    }
}

pub(super) fn clients_document(at: u64, scale: u64) -> Value {
    json!({
        "sampledAtUnixMs": at,
        "generation": 4,
        "ipv6Prefix": 64,
        "trackedClients": 3,
        "truncated": false,
        "clients": [
            {
                "ip": "192.0.2.10", "connections": 3, "connectionsTotal": 40 * scale, "requests": 900 * scale,
                "bytesIn": 4_000_000 * scale, "bytesOut": 90_000 * scale, "firstSeenUnixMs": 2_000,
                "lastActivityUnixMs": at - 100, "listeners": ["edge"], "protocols": ["http"], "errors": 9 * scale
            },
            {
                "ip": "198.51.100.7", "connections": 1, "connectionsTotal": 2, "requests": 20 * scale,
                "bytesIn": 2_000 * scale, "bytesOut": 800 * scale, "firstSeenUnixMs": 9_000,
                "lastActivityUnixMs": at - 1_000, "listeners": ["edge", "tls"], "protocols": ["http", "h2"],
                "errors": 0
            },
        ],
        "other": {
            "clients": 1, "connections": 1, "requests": 5 * scale, "bytesIn": 500 * scale, "bytesOut": 100 * scale
        },
        "totals": {
            "clients": 3, "connections": 5, "requests": 925 * scale, "bytesIn": 4_002_500 * scale,
            "bytesOut": 90_900 * scale
        },
    })
}

pub(super) fn conns_document(at: u64, scale: u64) -> Value {
    json!({
        "sampledAtUnixMs": at,
        "truncated": false,
        "total": 2,
        "connections": [
            {
                "id": "c-1", "peerIp": "192.0.2.10", "peerPort": 51_000, "listener": "edge", "protocol": "h2",
                "tls": { "version": "TLSv1.3", "sni": "app.example.com", "alpn": "h2" }, "state": "active",
                "ageMs": at * 6, "idleMs": 40, "requests": 12 * scale, "bytesIn": 40_000 * scale,
                "bytesOut": 9_000 * scale, "service": "site", "upstream": "web",
                "currentRequest": { "method": "GET", "path": "/slow", "ageMs": 1_500 }
            },
            {
                "id": "c-2", "peerIp": "198.51.100.7", "peerPort": 52_000, "listener": "edge", "protocol": "http",
                "tls": null, "state": "idle", "ageMs": 5_000, "idleMs": 4_000, "requests": scale,
                "bytesIn": 300 * scale, "bytesOut": 100 * scale, "service": null, "upstream": null,
                "currentRequest": null
            },
        ],
    })
}

pub(super) fn rtmp_document(at: u64, scale: u64) -> Value {
    json!({
        "asOfUnixMs": at,
        "global": {
            "activeStreams": 1, "publishers": 1, "subscribers": 1, "audioPayloadBytes": 1_000 * scale,
            "videoPayloadBytes": 9_000 * scale
        },
        "live": [{
            "id": "s-1", "service": "ingest", "application": "live", "name": "show", "createdAtUnixMs": 2_000,
            "subscriberCount": 1, "audioPayloadBytes": 1_000 * scale, "videoPayloadBytes": 9_000 * scale
        }],
        "clients": [
            {
                "id": "p-1", "service": "ingest", "peerIp": "192.0.2.20", "connectedAtUnixMs": 2_000,
                "application": "live", "stream": "show", "role": "publisher"
            },
            {
                "id": "v-1", "service": "ingest", "peerIp": "192.0.2.21", "connectedAtUnixMs": 5_000,
                "application": "live", "stream": "show", "role": "subscriber"
            },
        ],
        "liveTruncated": false,
        "clientsTruncated": false,
    })
}

fn feed<T: DeserializeOwned>(document: Value) -> Feed<T> {
    Feed::Ready(serde_json::from_value(document).expect("a document"))
}

pub(super) fn full(snapshot: StatsSnapshot, scale: u64) -> Sample {
    let at = snapshot.sampled_at_unix_ms;
    Sample {
        requests: feed(requests_document(&[
            ("req-old", 42_000),
            ("req-new", 1_500),
        ])),
        clients: feed(clients_document(at, scale)),
        conns: feed(conns_document(at, scale)),
        rtmp: feed(rtmp_document(at, scale)),
        ..sample(snapshot)
    }
}
