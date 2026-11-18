use super::*;
use http_support::HttpResponse;

async fn management(
    address: SocketAddr,
    method: &str,
    path: &str,
    range: Option<&str>,
) -> HttpResponse {
    let authorization = format!("Bearer {TOKEN}");
    let mut headers = vec![("Authorization", authorization.as_str())];
    if let Some(range) = range {
        headers.push(("Range", range));
    }
    http_request(address, method, path, &headers, &[]).await
}

pub(super) fn inventory_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    root: &Path,
    unused_root: &Path,
) -> ConfigDraft {
    let mut config = runtime_config(management_address, rtmp_address, root, unused_root);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = Some(4);
    config
}

pub(super) async fn publish_and_close(rtmp_address: SocketAddr, stream: &str, frames: u32) {
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish(stream).await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    for timestamp in 1..=frames {
        publisher
            .publish_audio(timestamp * 20, &[0xaf, 0x01, 0x40])
            .await;
    }
    publisher.close().await;
}

pub(super) async fn wait_for_recordings(
    address: SocketAddr,
    query: &str,
    predicate: impl Fn(&[Value]) -> bool,
) -> Value {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let response = management(
                address,
                "GET",
                &format!("/api/v1/rtmp/recordings{query}"),
                None,
            )
            .await;
            assert_eq!(response.status, 200);
            let listing = response.json();
            if predicate(listing["recordings"].as_array().expect("recordings")) {
                return listing;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recording inventory condition timed out")
}

#[tokio::test]
async fn recordings_are_listed_downloaded_with_ranges_deleted_and_keep_their_ids_across_restart() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "inventory-recordings");
    let unused = create_secure_root(directory.path(), "inventory-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = inventory_config(management_address, rtmp_address, &root, &unused);
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    publish_and_close(rtmp_address, "camera", 12).await;
    let listing = wait_for_recordings(management_address, "?stream=camera", |recordings| {
        recordings.len() >= 3 && recordings.iter().all(|entry| entry["status"] == "complete")
    })
    .await;
    let recordings = listing["recordings"].as_array().expect("recordings");
    let first = &recordings[0];
    assert_eq!(first["stream"], "camera");
    assert_eq!(first["audio_codec"], "aac");
    assert_eq!(first["recorders"][0]["recorder"], "archive");
    assert!(first["started_at_unix_ms"].is_u64());
    assert!(first["duration_ms"].is_u64());
    let name = first["name"].as_str().expect("name");
    let on_disk = fs::read(root.join(name)).expect("recording file");
    assert_eq!(first["size_bytes"], on_disk.len().to_string());
    assert_eq!(
        listing["roots"][0]["files"].as_u64(),
        Some(recordings.len() as u64)
    );
    let id = first["id"].as_str().expect("id").to_owned();

    let metadata = management(
        management_address,
        "GET",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(metadata.status, 200);
    assert_eq!(metadata.json()["id"], id);

    let content_path = format!("/api/v1/rtmp/recordings/{id}/content");
    let full = management(management_address, "GET", &content_path, None).await;
    assert_eq!(full.status, 200);
    assert_eq!(full.body, on_disk);
    assert!(full.headers["content-disposition"].starts_with("attachment; filename=\""));
    assert_eq!(full.headers["accept-ranges"], "bytes");
    let partial = management(management_address, "GET", &content_path, Some("bytes=2-9")).await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, on_disk[2..10]);
    assert_eq!(
        partial.headers["content-range"],
        format!("bytes 2-9/{}", on_disk.len())
    );
    let beyond = format!("bytes={}-", on_disk.len());
    let invalid = management(management_address, "GET", &content_path, Some(&beyond)).await;
    assert_eq!(invalid.status, 416);

    server.shutdown();
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let restarted = wait_for_recordings(management_address, "?stream=camera", |recordings| {
        !recordings.is_empty()
    })
    .await;
    assert!(
        restarted["recordings"]
            .as_array()
            .expect("recordings")
            .iter()
            .any(|entry| entry["id"] == id.as_str() && entry["status"] == "complete"),
        "the recording id must survive a restart"
    );

    let deleted = management(
        management_address,
        "DELETE",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.json()["deleted"]["id"], id);
    assert!(!root.join(name).exists());
    let gone = management(
        management_address,
        "GET",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(gone.status, 404);
    server.shutdown();
}

#[tokio::test]
async fn an_active_recording_is_listed_as_recording_and_cannot_be_deleted() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "active-recordings");
    let unused = create_secure_root(directory.path(), "active-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(management_address, rtmp_address, &root, &unused);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("live-camera").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;

    let listing = wait_for_recordings(management_address, "?status=recording", |recordings| {
        recordings.len() == 1
    })
    .await;
    let id = listing["recordings"][0]["id"].as_str().expect("id");
    let refused = management(
        management_address,
        "DELETE",
        &format!("/api/v1/rtmp/recordings/{id}"),
        None,
    )
    .await;
    assert_eq!(refused.status, 409);
    assert_eq!(refused.json()["error"]["code"], "recording_active");
    publisher.close().await;
    server.shutdown();
}

#[tokio::test]
async fn a_recording_cut_short_by_a_crash_is_listed_partial_after_restart() {
    let directory = TempDir::new().expect("recording directory");
    let root = create_secure_root(directory.path(), "crash-recordings");
    let unused = create_secure_root(directory.path(), "crash-unused");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let mut config = runtime_config(management_address, rtmp_address, &root, &unused);
    config.rtmp_services[0].applications[0].recorders[0].max_frames = None;
    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;
    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher.publish("crashed").await;
    publisher.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    publisher.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    wait_for_recordings(management_address, "?status=recording", |recordings| {
        recordings.len() == 1
    })
    .await;
    let killed = std::process::Command::new("kill")
        .args(["-9", &server.pid().to_string()])
        .status()
        .expect("kill the daemon");
    assert!(killed.success());
    drop(server);
    drop(publisher);

    let mut server = ServerProcess::start(&config.clone().validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let listing = wait_for_recordings(management_address, "?stream=crashed", |recordings| {
        recordings.len() == 1
    })
    .await;
    assert_eq!(listing["recordings"][0]["status"], "partial");
    let name = listing["recordings"][0]["name"]
        .as_str()
        .expect("name")
        .to_owned();

    let mut again = RtmpWireClient::connect(rtmp_address, "continuous").await;
    again.publish("crashed").await;
    again.publish_audio(0, &[0xaf, 0x00, 0x12]).await;
    again.publish_audio(20, &[0xaf, 0x01, 0x40]).await;
    let catalog = wait_for_catalog(management_address, |catalog| {
        stream_for(catalog, "continuous").is_some_and(|stream| {
            stream["recorders"][0]["recoverable_partial_name"] == name.as_str()
        })
    })
    .await;
    assert_eq!(
        stream_for(&catalog, "continuous").expect("stream")["recorders"][0]["recoverable_partial_name"],
        name.as_str()
    );
    again.close().await;
    server.shutdown();
}

struct TicketDaemon {
    server: ServerProcess,
    address: SocketAddr,
    id: String,
    other_id: String,
    on_disk: Vec<u8>,
    _directory: TempDir,
}

impl TicketDaemon {
    async fn start() -> Self {
        let directory = TempDir::new().expect("recording directory");
        let root = create_secure_root(directory.path(), "ticket-recordings");
        let unused = create_secure_root(directory.path(), "ticket-unused");
        let address = reserve_tcp_address();
        let rtmp_address = reserve_tcp_address();
        let config = inventory_config(address, rtmp_address, &root, &unused);
        let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
        server.wait_for_tcp(address).await;
        server.wait_for_tcp(rtmp_address).await;
        publish_and_close(rtmp_address, "camera", 12).await;
        let listing = wait_for_recordings(address, "?stream=camera", |recordings| {
            recordings.len() >= 2 && recordings.iter().all(|entry| entry["status"] == "complete")
        })
        .await;
        let recordings = &listing["recordings"];
        let on_disk = fs::read(root.join(recordings[0]["name"].as_str().expect("name")))
            .expect("recording file");
        Self {
            server,
            address,
            id: recordings[0]["id"].as_str().expect("id").to_owned(),
            other_id: recordings[1]["id"].as_str().expect("id").to_owned(),
            on_disk,
            _directory: directory,
        }
    }

    async fn mint(&self, id: &str, body: &str) -> HttpResponse {
        let authorization = format!("Bearer {TOKEN}");
        let headers = [
            ("Authorization", authorization.as_str()),
            ("Content-Type", "application/json"),
        ];
        let path = format!("/api/v1/rtmp/recordings/{id}/ticket");
        http_request(self.address, "POST", &path, &headers, body.as_bytes()).await
    }

    async fn ticketed_path(&self) -> (String, String) {
        let minted = self.mint(&self.id, "").await;
        assert_eq!(minted.status, 200, "{}", minted.text());
        let body = minted.json();
        let ticket = body["ticket"].as_str().expect("ticket").to_owned();
        (ticket, body["path"].as_str().expect("path").to_owned())
    }

    async fn anonymous(&self, method: &str, path: &str) -> HttpResponse {
        http_request(self.address, method, path, &[], &[]).await
    }

    fn content_path(id: &str) -> String {
        format!("/api/v1/rtmp/recordings/{id}/content")
    }
}

#[tokio::test]
async fn a_recording_ticket_downloads_the_file_with_ranges_and_without_the_bearer_token() {
    let daemon = TicketDaemon::start().await;
    let minted = daemon.mint(&daemon.id, "").await;
    assert_eq!(minted.status, 200, "{}", minted.text());
    assert_eq!(minted.header("cache-control"), Some("no-store"));
    assert_eq!(minted.header("referrer-policy"), Some("no-referrer"));
    let body = minted.json();
    let ticket = body["ticket"].as_str().expect("ticket");
    let path = body["path"].as_str().expect("path");
    assert_eq!(body["ttlSeconds"], 90);
    assert!(body["expiresAtUnixMs"].is_u64());
    assert_eq!(
        path,
        format!("{}?ticket={ticket}", TicketDaemon::content_path(&daemon.id))
    );

    let download = daemon.anonymous("GET", path).await;
    assert_eq!(download.status, 200);
    assert_eq!(download.body, daemon.on_disk);
    assert_eq!(download.header("cache-control"), Some("no-store"));
    assert_eq!(download.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(download.header("accept-ranges"), Some("bytes"));
    assert!(
        download
            .header("content-disposition")
            .is_some_and(|value| value.starts_with("attachment; filename=\""))
    );
    let range = [("Range", "bytes=2-9")];
    let ranged = http_request(daemon.address, "GET", path, &range, &[]).await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.body, daemon.on_disk[2..10]);
    assert_eq!(ranged.header("referrer-policy"), Some("no-referrer"));

    let authorization = format!("Bearer {TOKEN}");
    let bearer = [("Authorization", authorization.as_str())];
    let plain = http_request(daemon.address, "GET", path, &bearer, &[]).await;
    assert_eq!(plain.status, 200);
    assert_eq!(plain.header("referrer-policy"), None);
    daemon.server.shutdown();
}

#[tokio::test]
async fn a_recording_ticket_works_only_on_the_content_of_its_own_recording() {
    let daemon = TicketDaemon::start().await;
    let (ticket, _) = daemon.ticketed_path().await;
    let id = &daemon.id;
    for refused in [
        format!(
            "{}?ticket={ticket}",
            TicketDaemon::content_path(&daemon.other_id)
        ),
        format!("{}?ticket={ticket}A", TicketDaemon::content_path(id)),
        format!(
            "{}?ticket={ticket}&ticket={ticket}",
            TicketDaemon::content_path(id)
        ),
        format!("/api/v1/rtmp/recordings/{id}?ticket={ticket}"),
        format!("/api/v1/rtmp/recordings?ticket={ticket}"),
        format!("/api/v1/rtmp/streams?ticket={ticket}"),
    ] {
        let response = daemon.anonymous("GET", &refused).await;
        assert_eq!(response.status, 401, "{refused}");
    }
    let forged = format!("{}?ticket={ticket}A", TicketDaemon::content_path(id));
    let forged = daemon.anonymous("GET", &forged).await;
    assert_eq!(forged.json()["error"]["code"], "invalid_ticket");
    assert_eq!(forged.header("referrer-policy"), None);

    let delete = format!("/api/v1/rtmp/recordings/{id}?ticket={ticket}");
    assert_eq!(daemon.anonymous("DELETE", &delete).await.status, 401);
    let item = format!("/api/v1/rtmp/recordings/{id}");
    assert_eq!(
        management(daemon.address, "GET", &item, None).await.status,
        200
    );
    daemon.server.shutdown();
}

#[tokio::test]
async fn minting_a_recording_ticket_checks_the_recording_and_lifetime_and_hides_the_ticket() {
    let daemon = TicketDaemon::start().await;
    let (ticket, _) = daemon.ticketed_path().await;
    let unknown = daemon.mint("missing", "").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json()["error"]["code"], "recording_not_found");
    for invalid in [r#"{"ttlSeconds":0}"#, r#"{"ttlSeconds":601}"#] {
        let response = daemon.mint(&daemon.id, invalid).await;
        assert_eq!(response.status, 400, "{invalid}");
        assert_eq!(response.json()["error"]["code"], "invalid_ticket_ttl");
    }
    let path = format!("/api/v1/rtmp/recordings/{}/ticket", daemon.id);
    assert_eq!(daemon.anonymous("POST", &path).await.status, 401);

    let audit = management(daemon.address, "GET", "/api/v1/audit?limit=1000", None).await;
    assert!(
        audit.text().contains("media_ticket_mint"),
        "{}",
        audit.text()
    );
    let mac = ticket.split_once('.').expect("ticket shape").1;
    for page in [audit.text(), daemon.server.stderr()] {
        assert!(!page.contains(mac));
        assert!(!page.contains("ticket="));
    }
    daemon.server.shutdown();
}

#[tokio::test]
async fn an_expired_recording_ticket_is_refused_and_a_deleted_recording_has_nothing_to_download() {
    let daemon = TicketDaemon::start().await;
    let minted = daemon.mint(&daemon.id, r#"{"ttlSeconds":1}"#).await;
    assert_eq!(minted.status, 200);
    let path = minted.json()["path"].as_str().expect("path").to_owned();
    let expired = timeout(WIRE_TIMEOUT, async {
        loop {
            let response = daemon.anonymous("GET", &path).await;
            if response.status != 200 {
                return response;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the ticket did not expire");
    assert_eq!(expired.status, 401);
    assert_eq!(expired.json()["error"]["code"], "ticket_expired");

    let (_, fresh_path) = daemon.ticketed_path().await;
    let item = format!("/api/v1/rtmp/recordings/{}", daemon.id);
    assert_eq!(
        management(daemon.address, "DELETE", &item, None)
            .await
            .status,
        200
    );
    let gone = daemon.anonymous("GET", &fresh_path).await;
    assert_eq!(gone.status, 404);
    assert_eq!(gone.json()["error"]["code"], "recording_not_found");
    daemon.server.shutdown();
}
