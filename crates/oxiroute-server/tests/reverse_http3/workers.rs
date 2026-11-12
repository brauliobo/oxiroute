//! A reverse HTTP/3 listener with several workers runs several QUIC endpoints on its one address, and the clients
//! it answers are spread over them.
use super::origins::{AUTHORITY, Daemon, draft};
#[allow(clippy::wildcard_imports)]
use super::*;

const ENDPOINT_THREAD: &str = "oxiroute-quic-e";

/// The process and every process under it: the supervised master starts a launcher that starts the worker.
fn with_descendants(pid: u32) -> Vec<u32> {
    let parents: Vec<(u32, u32)> = std::fs::read_dir("/proc")
        .expect("the process table")
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|process| {
            let stat = std::fs::read_to_string(format!("/proc/{process}/stat")).ok()?;
            let after_name = stat.rsplit_once(')')?.1;
            Some((process, after_name.split_whitespace().nth(1)?.parse().ok()?))
        })
        .collect();
    let mut all = vec![pid];
    let mut next = 0;
    while let Some(&parent) = all.get(next) {
        all.extend(
            parents
                .iter()
                .filter(|(_, of)| *of == parent)
                .map(|(process, _)| *process),
        );
        next += 1;
    }
    all
}

/// The time each QUIC endpoint thread of the server has spent running, in nanoseconds.
fn endpoint_run_times(pid: u32) -> Vec<u64> {
    with_descendants(pid)
        .into_iter()
        .flat_map(|pid| {
            std::fs::read_dir(format!("/proc/{pid}/task"))
                .into_iter()
                .flatten()
        })
        .filter_map(Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.starts_with(ENDPOINT_THREAD))
        })
        .map(|task| {
            let schedstat = std::fs::read_to_string(task.path().join("schedstat"))
                .expect("the scheduler statistics of a thread");
            schedstat
                .split_whitespace()
                .next()
                .and_then(|nanoseconds| nanoseconds.parse().ok())
                .expect("run time")
        })
        .collect()
}

/// One request from a client of its own: a new UDP socket, so a new peer address.
async fn answer_to_new_client(listener: SocketAddr) -> u16 {
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let mut stream = sender
        .send_request(
            Request::builder()
                .method("GET")
                .uri(format!("https://{AUTHORITY}/"))
                .body(())
                .expect("H3 request"),
        )
        .await
        .expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let status = stream.recv_response().await.expect("H3 response").status();
    recv_body(&mut stream).await;
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"done");
    driver.abort();
    status.as_u16()
}

#[tokio::test]
pub(super) async fn a_listener_with_workers_answers_clients_through_several_endpoints() {
    let listener = reserve_udp_address();
    let mut draft = draft(listener, fixed_service(), Vec::new());
    draft.listener_workers = Some(4);
    draft.listeners[0].max_connections = Some(256);
    let config = draft
        .validate()
        .expect("valid reverse HTTP/3 configuration");
    let mut daemon = Daemon::start(listener, &config).await;
    assert_eq!(daemon.get("/").await.status, 200);

    let before = endpoint_run_times(daemon.server.pid());
    assert_eq!(before.len(), 4, "one QUIC endpoint per worker");
    for _ in 0..48 {
        assert_eq!(answer_to_new_client(listener).await, 200);
    }
    let after = endpoint_run_times(daemon.server.pid());
    let busy = before
        .iter()
        .zip(&after)
        .filter(|(before, after)| **after > **before + 500_000)
        .count();
    assert!(
        busy >= 2,
        "the clients were spread over the endpoints: {before:?} then {after:?}"
    );
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn a_listener_with_one_worker_runs_one_quic_endpoint() {
    let listener = reserve_udp_address();
    let config = draft(listener, fixed_service(), Vec::new())
        .validate()
        .expect("valid reverse HTTP/3 configuration");
    let mut daemon = Daemon::start(listener, &config).await;
    assert_eq!(daemon.get("/").await.status, 200);
    assert_eq!(endpoint_run_times(daemon.server.pid()).len(), 1);
    daemon.finish().await;
}
