use std::sync::{Arc, Mutex};

use oxiroute_config::RtmpNotifyMethod;
use serde_json::json;

use super::*;
use http_support::{HttpResponse, read_request_head};

const PUBLISH_SECRET: &str = "demo-secret-not-real";
const PLAY_SECRET: &str = "demo-viewer-secret-not-real";
const CALLBACK_KEY: &str = "demo-callback-key-not-real";
const ROTATED_SECRET: &str = "demo-rotated-secret-not-real";
const PUBLISH_FINGERPRINT: &str = "fp-8c6959af";
const PLAY_FINGERPRINT: &str = "fp-5d06566e";
const SECRETS: [&str; 3] = [PUBLISH_SECRET, PLAY_SECRET, CALLBACK_KEY];

type Captured = Arc<Mutex<Vec<String>>>;

async fn callback_listener() -> (SocketAddr, Captured) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("callback listener bind");
    let address = listener.local_addr().expect("callback listener address");
    let captured = Captured::default();
    let sink = Arc::clone(&captured);
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("callback accept");
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let mut request = read_request_head(&mut stream).await.expect("callback head");
                let text = String::from_utf8_lossy(&request).into_owned();
                let head_end = text.find("\r\n\r\n").expect("callback head end") + 4;
                let length = text
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or_default();
                while request.len() < head_end + length {
                    let mut chunk = [0; 1024];
                    let read = stream.read(&mut chunk).await.expect("callback body");
                    request.extend_from_slice(&chunk[..read]);
                }
                sink.lock()
                    .expect("callback sink")
                    .push(String::from_utf8_lossy(&request).into_owned());
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .expect("callback response");
            });
        }
    });
    (address, captured)
}

fn secured_config(
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    callback_address: SocketAddr,
    method: RtmpNotifyMethod,
    access_log: &Path,
) -> ConfigDraft {
    let mut config = runtime_config(
        management_address,
        rtmp_address,
        Path::new("/unused"),
        Path::new("/unused"),
    );
    let service = &mut config.rtmp_services[0];
    service.outbound_policy.deny_private = false;
    service.access_log = Some(AccessLogPolicy::File {
        path: access_log.to_owned(),
    });
    service.applications.truncate(1);
    let application = &mut service.applications[0];
    application.recorders.clear();
    for (policy, parameter, secret) in [
        (&mut application.publish, "token", PUBLISH_SECRET),
        (&mut application.play, "viewer", PLAY_SECRET),
    ] {
        policy.token = Some(oxiroute_config::RtmpTokenPolicy {
            source: oxiroute_config::RtmpTokenSource::StreamQuery,
            parameter: parameter.into(),
            secret: secret.into(),
        });
    }
    let url = |path: &str| format!("http://{callback_address}/{path}?key={CALLBACK_KEY}");
    application.callbacks.on_publish = Some(url("publish"));
    application.callbacks.on_play = Some(url("play"));
    application.callbacks.on_publish_done = Some(url("publish_done"));
    application.callbacks.on_play_done = Some(url("play_done"));
    application.callbacks.notify_method = method;
    config
}

fn authorization() -> String {
    format!("Bearer {TOKEN}")
}

async fn management(
    address: SocketAddr,
    method: &str,
    path: &str,
    revision: Option<&str>,
    body: Option<&Value>,
) -> HttpResponse {
    let authorization = authorization();
    let mut headers = vec![("Authorization", authorization.as_str())];
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    if let Some(revision) = revision {
        headers.push(("If-Config-Revision", revision));
    }
    let body = body.map(Value::to_string).unwrap_or_default();
    http_request(address, method, path, &headers, body.as_bytes()).await
}

async fn wait_for_callbacks(captured: &Captured, count: usize) -> Vec<String> {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let requests = captured.lock().expect("callback sink").clone();
            if requests.len() >= count {
                return requests;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("callback requests timed out")
}

async fn assert_stream_secret_absent_everywhere(method: RtmpNotifyMethod) {
    let log_directory = TempDir::new().expect("access log directory");
    let access_log = log_directory.path().join("rtmp-access.jsonl");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let (callback_address, captured) = callback_listener().await;
    let config = secured_config(
        management_address,
        rtmp_address,
        callback_address,
        method,
        &access_log,
    );
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher
        .publish(&format!("camera?token={PUBLISH_SECRET}&quality=hd"))
        .await;
    let mut viewer = RtmpWireClient::connect(rtmp_address, "continuous").await;
    viewer.play(&format!("camera?viewer={PLAY_SECRET}")).await;
    viewer.close().await;
    publisher.close().await;
    let requests = wait_for_callbacks(&captured, 4).await;

    let publish = requests
        .iter()
        .find(|request| request.contains("call=publish&") || request.contains("call=publish\r"))
        .expect("publish callback");
    let play = requests
        .iter()
        .find(|request| request.contains("call=play&") || request.contains("call=play\r"))
        .expect("play callback");
    assert!(publish.contains(&format!(
        "query=token%3D{PUBLISH_FINGERPRINT}%26quality%3Dhd"
    )));
    assert!(play.contains(&format!("query=viewer%3D{PLAY_FINGERPRINT}")));
    for request in &requests {
        for secret in [PUBLISH_SECRET, PLAY_SECRET] {
            assert!(!request.contains(secret), "{request}");
        }
        let request_line = request.lines().next().expect("request line");
        assert_eq!(
            request_line.starts_with("GET "),
            method == RtmpNotifyMethod::Get,
            "{request_line}"
        );
    }

    let mut surfaces = Vec::new();
    for path in [
        "/api/v1/config",
        "/api/v1/status",
        "/api/v1/topology",
        "/api/v1/monitoring",
        "/api/v1/rtmp/streams",
        "/api/v1/audit?limit=200",
        "/api/v2/events",
    ] {
        let response = management(management_address, "GET", path, None, None).await;
        assert_eq!(response.status, 200, "{path}");
        surfaces.push((path, response.text()));
    }
    let events = &surfaces.last().expect("events surface").1;
    assert!(events.contains("rtmp_publish"), "{events}");
    let configuration = surfaces[0].1.as_str();
    assert!(configuration.contains(&format!("http://{callback_address}/publish?key=<redacted>")));
    let log_text = server.shutdown_output();
    surfaces.push(("standard error", log_text));
    surfaces.push((
        "access log",
        fs::read_to_string(&access_log).expect("flushed RTMP access log"),
    ));
    for (surface, text) in &surfaces {
        for secret in SECRETS {
            assert!(!text.contains(secret), "{surface} leaks {secret}");
        }
    }
}

#[tokio::test]
async fn callback_posts_and_management_surfaces_never_carry_the_stream_secret() {
    assert_stream_secret_absent_everywhere(RtmpNotifyMethod::Post).await;
}

#[tokio::test]
async fn callback_get_urls_and_management_surfaces_never_carry_the_stream_secret() {
    assert_stream_secret_absent_everywhere(RtmpNotifyMethod::Get).await;
}

fn callback_fingerprint(requests: &[String], call: &str, parameter: &str) -> String {
    let marker = format!("query={parameter}%3D");
    let request = requests
        .iter()
        .find(|request| {
            request.contains(&format!("call={call}&"))
                || request.contains(&format!("call={call}\r"))
        })
        .unwrap_or_else(|| panic!("{call} callback"));
    let start = request.find(&marker).expect("fingerprinted query") + marker.len();
    request[start..start + "fp-00000000".len()].to_owned()
}

#[tokio::test]
async fn the_secret_listing_carries_the_fingerprints_the_callbacks_show_and_never_the_secrets() {
    let log_directory = TempDir::new().expect("access log directory");
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let (callback_address, captured) = callback_listener().await;
    let config = secured_config(
        management_address,
        rtmp_address,
        callback_address,
        RtmpNotifyMethod::Post,
        &log_directory.path().join("rtmp-access.jsonl"),
    );
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(rtmp_address).await;

    let mut publisher = RtmpWireClient::connect(rtmp_address, "continuous").await;
    publisher
        .publish(&format!("camera?token={PUBLISH_SECRET}"))
        .await;
    let mut viewer = RtmpWireClient::connect(rtmp_address, "continuous").await;
    viewer.play(&format!("camera?viewer={PLAY_SECRET}")).await;
    viewer.close().await;
    publisher.close().await;
    let requests = wait_for_callbacks(&captured, 4).await;
    let publish_fingerprint = callback_fingerprint(&requests, "publish", "token");
    let play_fingerprint = callback_fingerprint(&requests, "play", "viewer");

    let listing = management(
        management_address,
        "GET",
        "/api/v1/rtmp/secrets",
        None,
        None,
    )
    .await;
    assert_eq!(listing.status, 200, "{}", listing.text());
    assert_eq!(listing.header("cache-control"), Some("no-store"));
    assert_eq!(
        listing.json(),
        json!({ "secrets": [
            {
                "service": "wire-rtmp",
                "application": "continuous",
                "role": "play",
                "fingerprint": play_fingerprint
            },
            {
                "service": "wire-rtmp",
                "application": "continuous",
                "role": "publish",
                "fingerprint": publish_fingerprint
            },
        ] })
    );
    assert_eq!(publish_fingerprint, PUBLISH_FINGERPRINT);
    assert_eq!(play_fingerprint, PLAY_FINGERPRINT);

    let unauthenticated =
        http_request(management_address, "GET", "/api/v1/rtmp/secrets", &[], &[]).await;
    assert_eq!(unauthenticated.status, 401);
    let method = management(
        management_address,
        "POST",
        "/api/v1/rtmp/secrets",
        None,
        None,
    )
    .await;
    assert_eq!(method.status, 405);

    let mut surfaces = vec![("secret listing", listing.text())];
    for path in ["/api/v1/audit?limit=200", "/api/v2/events"] {
        let response = management(management_address, "GET", path, None, None).await;
        surfaces.push((path, response.text()));
    }
    surfaces.push(("standard error", server.shutdown_output()));
    for (surface, text) in &surfaces {
        for secret in SECRETS {
            assert!(!text.contains(secret), "{surface} leaks {secret}");
        }
    }
}

#[tokio::test]
async fn applications_without_a_key_policy_list_no_fingerprints() {
    let management_address = reserve_tcp_address();
    let rtmp_address = reserve_tcp_address();
    let config = idle_runtime_config(management_address, rtmp_address);
    let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let listing = management(
        management_address,
        "GET",
        "/api/v1/rtmp/secrets",
        None,
        None,
    )
    .await;
    assert_eq!(listing.status, 200);
    assert_eq!(listing.json(), json!({ "secrets": [] }));
    server.shutdown();
}

struct SecuredDaemon {
    server: ServerProcess,
    management_address: SocketAddr,
    rtmp_address: SocketAddr,
    _log_directory: TempDir,
}

impl SecuredDaemon {
    async fn start() -> Self {
        let log_directory = TempDir::new().expect("access log directory");
        let management_address = reserve_tcp_address();
        let rtmp_address = reserve_tcp_address();
        let (callback_address, _captured) = callback_listener().await;
        let config = secured_config(
            management_address,
            rtmp_address,
            callback_address,
            RtmpNotifyMethod::Post,
            &log_directory.path().join("rtmp-access.jsonl"),
        );
        let mut server = ServerProcess::start(&config.validate().unwrap(), Some(TOKEN));
        server.wait_for_tcp(management_address).await;
        server.wait_for_tcp(rtmp_address).await;
        Self {
            server,
            management_address,
            rtmp_address,
            _log_directory: log_directory,
        }
    }

    async fn snapshot(&self) -> Value {
        management(self.management_address, "GET", "/api/v1/config", None, None)
            .await
            .json()
    }

    async fn put(&self, snapshot: &Value, config: Value) -> HttpResponse {
        let revision = snapshot["diskRevision"].as_str().expect("disk revision");
        management(
            self.management_address,
            "PUT",
            "/api/v1/config",
            Some(revision),
            Some(&json!({ "config": config })),
        )
        .await
    }

    async fn put_and_activate(&self, config: Value) -> HttpResponse {
        let original = active_revision(self.management_address, &authorization()).await;
        let saved = self.put(&self.snapshot().await, config).await;
        assert_eq!(saved.status, 200, "{}", saved.text());
        assert_ne!(saved.json()["outcome"], "unchanged_active");
        wait_for_new_revision(self.management_address, &authorization(), &original).await;
        saved
    }

    fn persisted(&self) -> String {
        fs::read_to_string(&self.server.config_path).expect("persisted configuration")
    }

    async fn publish_is_accepted(&self, stream: &str) -> bool {
        let mut client = RtmpWireClient::connect(self.rtmp_address, "continuous").await;
        let accepted = client.publish_is_accepted(stream).await;
        client.close().await;
        accepted
    }
}

fn application_json(config: &mut Value) -> &mut Value {
    &mut config["rtmp_services"][0]["applications"][0]
}

#[tokio::test]
async fn redacted_config_round_trip_keeps_every_stored_secret() {
    let daemon = SecuredDaemon::start().await;
    let snapshot = daemon.snapshot().await;
    let redacted = snapshot["config"].clone();
    assert_eq!(
        redacted["rtmp_services"][0]["applications"][0]["publish"]["token"]["secret"],
        "<redacted>"
    );
    assert!(!snapshot.to_string().contains(PUBLISH_SECRET));
    assert!(!snapshot.to_string().contains(CALLBACK_KEY));

    let unchanged = daemon.put(&snapshot, redacted.clone()).await;
    assert_eq!(unchanged.status, 200, "{}", unchanged.text());
    assert_eq!(unchanged.json()["outcome"], "unchanged_active");
    let mut edited = redacted;
    edited["rtmp_services"][0]["outbound_chunk_size"] = json!(8_192);
    let saved = daemon.put_and_activate(edited).await;
    assert!(!saved.text().contains(PUBLISH_SECRET));

    let persisted = daemon.persisted();
    assert!(persisted.contains("outbound_chunk_size = 8192"));
    for secret in SECRETS {
        assert!(persisted.contains(secret));
    }
    assert!(
        daemon
            .publish_is_accepted(&format!("camera?token={PUBLISH_SECRET}"))
            .await
    );
    assert!(!daemon.publish_is_accepted("camera").await);
    daemon.server.shutdown();
}

#[tokio::test]
async fn a_literal_value_next_to_a_placeholder_rotates_only_that_secret() {
    let daemon = SecuredDaemon::start().await;
    let mut rotated = daemon.snapshot().await["config"].clone();
    application_json(&mut rotated)["publish"]["token"]["secret"] = json!(ROTATED_SECRET);
    daemon.put_and_activate(rotated).await;

    let persisted = daemon.persisted();
    assert!(persisted.contains(ROTATED_SECRET));
    assert!(!persisted.contains(PUBLISH_SECRET));
    assert!(persisted.contains(PLAY_SECRET) && persisted.contains(CALLBACK_KEY));
    assert!(
        !daemon
            .publish_is_accepted(&format!("camera?token={PUBLISH_SECRET}"))
            .await
    );
    assert!(
        daemon
            .publish_is_accepted(&format!("camera?token={ROTATED_SECRET}"))
            .await
    );
    daemon.server.shutdown();
}

#[tokio::test]
async fn placeholders_that_match_nothing_stored_are_refused_and_write_nothing() {
    let daemon = SecuredDaemon::start().await;
    let snapshot = daemon.snapshot().await;
    let before = fs::read(&daemon.server.config_path).expect("configuration before rejected saves");

    let mut added = snapshot["config"].clone();
    let mut fresh = application_json(&mut added).clone();
    fresh["name"] = json!("fresh");
    added["rtmp_services"][0]["applications"]
        .as_array_mut()
        .expect("applications")
        .push(fresh);
    let unresolved = daemon.put(&snapshot, added).await;
    assert_eq!(unresolved.status, 422);
    assert_eq!(
        unresolved.json()["error"]["code"],
        "redacted_rtmp_token_unresolved"
    );

    let mut redirected = snapshot["config"].clone();
    application_json(&mut redirected)["callbacks"]["on_publish"] =
        json!("http://192.0.2.1/publish?key=<redacted>");
    let unresolved = daemon.put(&snapshot, redirected).await;
    assert_eq!(unresolved.status, 422);
    assert_eq!(
        unresolved.json()["error"]["code"],
        "redacted_rtmp_callback_unresolved"
    );
    assert_eq!(
        fs::read(&daemon.server.config_path).expect("configuration after rejected saves"),
        before
    );
    daemon.server.shutdown();
}
