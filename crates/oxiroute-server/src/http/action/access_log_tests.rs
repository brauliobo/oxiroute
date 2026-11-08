use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::*;

#[test]
fn bearer_access_preserves_custom_header_challenge_and_cardinality() {
    let directory = tempfile::tempdir().expect("bearer token directory");
    let path = directory.path().join("route.token");
    let token = "route-token-0123456789abcdef0123456789abcdef";
    std::fs::write(&path, format!("{token}\n")).expect("bearer token file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("bearer token mode");
    let access = BearerTokenAccess::load(&HttpAccessPolicy::BearerTokenFile {
        token_file_path: path,
        header_name: "x-route-token".into(),
        realm: Some("private".into()),
    })
    .expect("bearer access");
    assert_eq!(access.challenge(), "Bearer realm=\"private\"");

    let name = HeaderName::from_static("x-route-token");
    let mut headers = HeaderMap::new();
    assert!(!access.authorizes(&headers));
    headers.append(name.clone(), HeaderValue::from_static("Bearer wrong"));
    assert!(!access.authorizes(&headers));
    headers.insert(
        name.clone(),
        HeaderValue::from_str(&format!("Bearer {token}")).expect("bearer header"),
    );
    assert!(access.authorizes(&headers));
    headers.append(
        name,
        HeaderValue::from_str(&format!("Bearer {token}")).expect("duplicate bearer header"),
    );
    assert!(!access.authorizes(&headers));
}

#[tokio::test]
async fn mixed_htpasswd_schemes_authenticate_each_user() {
    let directory = tempfile::tempdir().expect("htpasswd directory");
    let path = directory.path().join("mixed.htpasswd");
    let apr1 = String::from_utf8(apr1_digest(b"alpha-pass", b"salt").unwrap().to_vec())
        .expect("APR1 digest");
    let bcrypt = bcrypt::hash_with_salt(b"bravo-pass", 4, *b"OxiRouteTest1234")
        .unwrap()
        .to_string();
    std::fs::write(&path, format!("alpha:$apr1$salt${apr1}\nbravo:{bcrypt}\n"))
        .expect("htpasswd file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("htpasswd mode");
    let access = BasicHtpasswdAccess::load(&path, "private").expect("mixed htpasswd");

    for (credentials, expected) in [
        ("alpha:alpha-pass", true),
        ("bravo:bravo-pass", true),
        ("alpha:wrong", false),
        ("missing:alpha-pass", false),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                STANDARD.encode(credentials.as_bytes())
            ))
            .unwrap(),
        );
        assert_eq!(access.authorizes(&headers).await, expected, "{credentials}");
    }

    let insensitive = BasicHtpasswdAccess::load_with_username_case(&path, "private", false)
        .expect("case-insensitive htpasswd");
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(b"ALPHA:alpha-pass"))).unwrap(),
    );
    assert!(insensitive.authorizes(&headers).await);

    let uppercase_path = directory.path().join("uppercase.htpasswd");
    std::fs::write(&uppercase_path, format!("ALPHA:$apr1$salt${apr1}\n"))
        .expect("uppercase htpasswd file");
    std::fs::set_permissions(&uppercase_path, std::fs::Permissions::from_mode(0o600))
        .expect("uppercase htpasswd mode");
    let uppercase = BasicHtpasswdAccess::load_with_username_case(&uppercase_path, "private", false)
        .expect("uppercase htpasswd");
    assert!(!uppercase.authorizes(&headers).await);
}

#[test]
fn static_etags_use_nginx_mtime_and_size_format() {
    assert_eq!(nginx_static_etag(0x1234, 0xabcd), "\"1234-abcd\"");
}

#[test]
fn access_log_rejects_a_symlinked_ancestor() {
    let directory = tempfile::tempdir().expect("access log fixture directory");
    let real = directory.path().join("real");
    std::fs::create_dir(&real).expect("real access log directory");
    let linked = directory.path().join("linked");
    symlink(&real, &linked).expect("symlinked access log directory");

    let policy = AccessLogPolicy::File {
        path: linked.join("access.jsonl"),
    };
    assert!(AccessLog::open("test", Some(&policy)).is_err());
    assert!(!real.join("access.jsonl").exists());
}

#[test]
fn access_log_validation_does_not_create_the_target() {
    let directory = tempfile::tempdir().expect("access log fixture directory");
    let path = directory.path().join("access.jsonl");
    let policy = AccessLogPolicy::File { path: path.clone() };

    AccessLog::validate(Some(&policy)).expect("access log validation");

    assert!(!path.exists());
}

#[test]
fn access_log_queue_saturation_never_blocks_the_caller() {
    assert_eq!(ACCESS_LOG_QUEUE_CAPACITY, 1_024);
    let access_log = AccessLog {
        sink: Arc::new(Sink::new(1)),
        service: "test".into(),
        rtmp_metrics: None,
        worker: Mutex::new(None),
    };
    let event = serde_json::json!({"status": 200});

    access_log.write(&event).expect("first queued event");
    let error = access_log.write(&event).expect_err("full queue rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn rtmp_access_log_queue_saturation_counts_nonblocking_drops() {
    let metrics = Arc::new(crate::observability::logging::RtmpAccessLogMetrics::default());
    let access_log = AccessLog {
        sink: Arc::new(Sink::new(1)),
        service: "test".into(),
        rtmp_metrics: Some(Arc::clone(&metrics)),
        worker: Mutex::new(None),
    };
    let event = serde_json::json!({"event": "connect"});

    access_log.write_rtmp(&event).expect("first queued event");
    let error = access_log
        .write_rtmp(&event)
        .expect_err("full RTMP queue rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.enqueued, 1);
    assert_eq!(snapshot.dropped, 1);
    assert_eq!(snapshot.queue_saturated, 1);
}

#[test]
fn rtmp_access_log_drop_flushes_the_bounded_redacted_record() {
    let directory = tempfile::tempdir().expect("RTMP access log directory");
    let path = directory.path().join("rtmp-access.jsonl");
    let access_log =
        AccessLog::open_rtmp("live", Some(&AccessLogPolicy::File { path: path.clone() }))
            .expect("RTMP access log preflight")
            .expect("RTMP file sink");
    access_log
        .write_rtmp(&serde_json::json!({
            "timestampUnixMs": 1,
            "event": "publish",
            "result": "accepted",
            "listener": "live-listener",
            "service": "live",
            "application": "camera",
            "stream": "feed",
            "sessionId": "session-1",
            "role": "publisher",
            "bytesReceived": 2,
            "bytesSent": 3,
            "messagesReceived": 4,
            "messagesSent": 5,
            "durationMs": 6,
            "failureCode": null,
            "query": "token=secret",
            "clientIp": "192.0.2.1",
        }))
        .expect("queue RTMP access event");
    drop(access_log);

    let contents = std::fs::read_to_string(path).expect("flushed RTMP access log");
    let record: serde_json::Value = serde_json::from_str(contents.trim()).expect("JSONL record");
    assert_eq!(record["event"], "publish");
    assert_eq!(record["messagesSent"], 5);
    assert!(record.get("query").is_none());
    assert!(record.get("clientIp").is_none());
    assert!(!contents.contains("secret"));
}

#[test]
fn access_log_writes_a_burst_as_complete_lines_in_order() {
    let directory = tempfile::tempdir().expect("access log directory");
    let path = directory.path().join("access.jsonl");
    let access_log = AccessLog::open("edge", Some(&AccessLogPolicy::File { path: path.clone() }))
        .expect("access log preflight")
        .expect("file sink");
    for index in 0..200 {
        access_log
            .write_record(&serde_json::json!({"seq": index}))
            .expect("queued line");
    }
    drop(access_log);

    let contents = std::fs::read_to_string(path).expect("flushed access log");
    let sequence: Vec<u64> = contents
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("JSONL")["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(sequence, (0..200).collect::<Vec<_>>());
    assert!(contents.ends_with('\n'));
}

#[test]
fn rtmp_access_log_counts_batches_as_received_and_written() {
    let directory = tempfile::tempdir().expect("RTMP access log directory");
    let path = directory.path().join("rtmp-access.jsonl");
    let metrics = Arc::new(crate::observability::logging::RtmpAccessLogMetrics::default());
    let access_log = AccessLog::open_with_rtmp_metrics(
        "live",
        Some(&AccessLogPolicy::File { path }),
        Some(Arc::clone(&metrics)),
    )
    .expect("RTMP access log preflight")
    .expect("RTMP file sink");
    for _ in 0..25 {
        access_log
            .write_rtmp(&serde_json::json!({"event": "connect"}))
            .expect("queued");
    }
    drop(access_log);

    let snapshot = metrics.snapshot();
    assert_eq!(
        (snapshot.enqueued, snapshot.written, snapshot.queue_depth),
        (25, 25, 0)
    );
}
