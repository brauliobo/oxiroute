//! Listener counters from the management API and the service access log.

use std::{fs, net::SocketAddr, path::Path, time::Duration};

use serde_json::{Value, json};
use tokio::time::sleep;

use crate::{config::MANAGEMENT_TOKEN, http_support};

const SETTLE_POLL: Duration = Duration::from_millis(150);

async fn monitoring(management: SocketAddr) -> Value {
    let authorization = format!("Bearer {MANAGEMENT_TOKEN}");
    let response = http_support::http_request(
        management,
        "GET",
        "/api/v1/monitoring",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(response.status, 200, "monitoring API");
    response.json()
}

fn listener_counters(monitoring: &Value) -> Value {
    let listener = monitoring["listeners"]
        .as_array()
        .and_then(|listeners| {
            listeners
                .iter()
                .find(|listener| listener["name"] == "reverse")
        })
        .expect("reverse listener snapshot");
    let operations = listener["httpOperations"]["outcomes"]
        .as_array()
        .into_iter()
        .flatten();
    let outcomes = operations
        .map(|entry| {
            (
                entry["result"].as_str().unwrap_or_default().to_owned(),
                entry["count"].clone(),
            )
        })
        .collect::<Vec<_>>();
    json!({
        "accepted_connections": listener["acceptedConnections"],
        "rejected_connections": listener["rejectedConnections"],
        "active_connections": listener["activeConnections"],
        "bytes_received": listener["bytesReceived"],
        "bytes_sent": listener["bytesSent"],
        "http_operations": outcomes.into_iter().collect::<serde_json::Map<_, _>>(),
        "cache": listener["cache"],
    })
}

pub async fn active_connections(management: SocketAddr) -> u64 {
    listener_counters(&monitoring(management).await)["active_connections"]
        .as_u64()
        .expect("active connections")
}

/// The counters once they stop changing: the server records an operation after the response is sent.
pub async fn settled(management: SocketAddr) -> Value {
    let mut previous = listener_counters(&monitoring(management).await);
    for _ in 0..40 {
        sleep(SETTLE_POLL).await;
        let current = listener_counters(&monitoring(management).await);
        if current == previous {
            return current;
        }
        previous = current;
    }
    panic!("listener counters did not settle");
}

/// `(authority host, "protocol service method status")` of every access log line, in order.
pub fn access_log(path: &Path) -> Vec<(Option<String>, String)> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("access log line"))
        .map(|line| {
            let text = |key: &str| line[key].as_str().unwrap_or("-").to_owned();
            let status = line["status"]
                .as_u64()
                .map_or_else(|| "-".into(), |status| status.to_string());
            let summary = format!(
                "{} {} {} {status}",
                text("protocol"),
                text("service"),
                text("method")
            );
            (line["host"].as_str().map(str::to_owned), summary)
        })
        .collect()
}

pub async fn settled_access_log(path: &Path) -> Vec<(Option<String>, String)> {
    let mut previous = access_log(path);
    for _ in 0..40 {
        sleep(SETTLE_POLL).await;
        let current = access_log(path);
        if current == previous {
            return current;
        }
        previous = current;
    }
    panic!("access log did not settle");
}
