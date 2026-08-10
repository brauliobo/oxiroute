#![allow(dead_code)]

use std::{net::SocketAddr, time::Duration};

use tokio::time::{sleep, timeout};

use crate::http_support::http_request;

const REVISION_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn active_revision(address: SocketAddr, authorization: &str) -> String {
    let response = http_request(
        address,
        "GET",
        "/api/v1/status",
        &[("Authorization", authorization)],
        &[],
    )
    .await;
    assert_eq!(response.status, 200);
    response.json()["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned()
}

pub async fn wait_for_new_revision(address: SocketAddr, authorization: &str, original: &str) {
    timeout(REVISION_TIMEOUT, async {
        loop {
            let revision = active_revision(address, authorization).await;
            if revision != original {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("generation reload timed out");
}
