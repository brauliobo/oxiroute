use crate::http_support::http_request;
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::shared::{TOKEN, management_config, public_pool};

#[tokio::test]
async fn management_serves_the_stats_snapshot_to_authenticated_clients_only() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    config.upstream_pools = vec![public_pool()];
    let mut server = ServerProcess::start(
        &config.validate().expect("valid process config"),
        Some(TOKEN),
    );
    server.wait_for_tcp(management_address).await;

    let anonymous = http_request(management_address, "GET", "/api/v1/stats", &[], &[]).await;
    assert_eq!(anonymous.status, 401);

    let authorization = format!("Bearer {TOKEN}");
    let headers = [("Authorization", authorization.as_str())];
    let snapshot = http_request(management_address, "GET", "/api/v1/stats", &headers, &[]).await;
    assert_eq!(snapshot.status, 200);
    let snapshot = snapshot.json();
    assert_eq!(snapshot["schemaVersion"], 1);
    assert_eq!(snapshot["workers"]["scaling"], "static");
    assert_eq!(snapshot["pools"][0]["name"], "public");
    assert_eq!(snapshot["pools"][0]["servers"][0]["name"], "origin-a");
    assert!(snapshot["process"]["uptimeMs"].is_u64());

    let post = http_request(management_address, "POST", "/api/v1/stats", &headers, &[]).await;
    assert_eq!(post.status, 405);
}
