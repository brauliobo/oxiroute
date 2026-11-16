use super::*;

#[tokio::test(flavor = "current_thread")]
async fn administrative_state_survives_file_writes_by_pool_and_server_name() {
    administrative_state_survives_file_writes(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn administrative_state_survives_supervised_worker_replacement() {
    administrative_state_survives_file_writes(true).await;
}

fn server_state(servers: &Value, name: &str) -> Value {
    servers["servers"]
        .as_array()
        .expect("servers")
        .iter()
        .map(|entry| entry["server"].clone())
        .find(|server| server["name"] == name)
        .unwrap_or_else(|| panic!("{name} is missing"))
}

fn override_method(path: &str) -> &'static str {
    if path.ends_with("max-connections") || path.ends_with("weight") {
        "PUT"
    } else {
        "POST"
    }
}

fn administrative_overrides() -> [(&'static str, Value); 5] {
    [
        (
            "/api/v1/servers/administrative-state",
            json!({"state": "drain"}),
        ),
        ("/api/v1/servers/health-override", json!({"health": "down"})),
        (
            "/api/v1/servers/max-connections",
            json!({"maxConnections": 7}),
        ),
        ("/api/v1/servers/weight", json!({"weight": 5})),
        ("/api/v1/servers/checks", json!({"enabled": false})),
    ]
}

fn assert_overrides_carried_and_defaults_added(servers: &Value) {
    let carried = server_state(servers, "origin-a");
    assert_eq!(carried["administrativeState"], "drain");
    assert_eq!(carried["healthOverride"], "down");
    assert_eq!(carried["maxConnections"], 7);
    assert_eq!(carried["weight"], 5);
    assert_eq!(carried["checksEnabled"], false);
    let added = server_state(servers, "origin-b");
    assert_eq!(added["administrativeState"], "ready");
    assert_eq!(added["healthOverride"], "auto");
    assert_eq!(added["checksEnabled"], true);
    assert!(added["maxConnections"].is_null());
    assert_eq!(added["weight"], 1);
}

async fn administrative_state_survives_file_writes(supervised: bool) {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    config.upstream_pools = vec![public_pool()];
    let validated = config.clone().validate().expect("valid process config");
    let mut server = if supervised {
        ServerProcess::start_supervised(&validated, TOKEN)
    } else {
        ServerProcess::start(&validated, Some(TOKEN))
    };
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let get = |path: &'static str| {
        let authorization = authorization.clone();
        async move {
            http_request(
                management_address,
                "GET",
                path,
                &[("Authorization", &authorization)],
                &[],
            )
            .await
            .json()
        }
    };
    let post = |path: &'static str, revision: Value, mut body: Value| {
        let authorization = authorization.clone();
        async move {
            body["expectedActiveRevision"] = revision;
            body["targets"] = json!([{"pool": "public", "server": "origin-a"}]);
            let response = http_request(
                management_address,
                override_method(path),
                path,
                &[
                    ("Authorization", &authorization),
                    ("Content-Type", "application/json"),
                ],
                &serde_json::to_vec(&body).expect("body"),
            )
            .await;
            assert_eq!(response.status, 200, "{}", response.text());
        }
    };
    let wait_for_revision = |previous: Value| {
        let get = &get;
        async move {
            let deadline = Instant::now() + process_support::PROCESS_TIMEOUT;
            loop {
                let status = get("/api/v1/generations").await["generation"].clone();
                if status["activeRevision"] != previous {
                    return status;
                }
                assert!(Instant::now() < deadline, "the file was not activated");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let status = get("/api/v1/generations").await["generation"].clone();
    let first_generation = status["generation"].as_u64().expect("generation number");
    let revision = status["activeRevision"].clone();
    for (path, body) in administrative_overrides() {
        post(path, revision.clone(), body).await;
    }

    let mut second = config.upstream_pools[0].servers[0].clone();
    second.name = "origin-b".into();
    second.endpoint = UpstreamEndpoint::Socket {
        address: "127.0.0.1:8081".parse().expect("upstream"),
    };
    config.upstream_pools[0].servers.push(second);
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
    let status = wait_for_revision(revision.clone()).await;
    assert!(status["generation"].as_u64().expect("number") > first_generation);
    assert_eq!(status["previousRevision"], revision);
    assert_overrides_carried_and_defaults_added(&get("/api/v1/servers").await);

    let origin_a = config.upstream_pools[0].servers.remove(0);
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
    let status = wait_for_revision(status["activeRevision"].clone()).await;
    config.upstream_pools[0].servers.insert(0, origin_a);
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
    wait_for_revision(status["activeRevision"].clone()).await;
    let returned = server_state(&get("/api/v1/servers").await, "origin-a");
    assert_eq!(returned["administrativeState"], "ready");
    assert_eq!(returned["healthOverride"], "auto");
    assert!(returned["maxConnections"].is_null());
    assert_eq!(returned["weight"], 1);

    server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_rejected_file_write_is_reported_until_the_next_accepted_file() {
    let management_address = reserve_tcp_address();
    let mut config = management_config(management_address, None);
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let get = |path: &'static str| {
        let authorization = authorization.clone();
        async move {
            http_request(
                management_address,
                "GET",
                path,
                &[("Authorization", &authorization)],
                &[],
            )
            .await
        }
    };
    let status = get("/api/v1/generations").await.json()["generation"].clone();
    assert!(status["lastRejection"].is_null());
    assert_eq!(status["degraded"], false);
    let active_revision = status["activeRevision"].clone();

    fs::write(
        &server.config_path,
        "return { version = secret_external_value",
    )
    .expect("bad write");
    let deadline = Instant::now() + process_support::PROCESS_TIMEOUT;
    let status = loop {
        let status = get("/api/v1/generations").await.json()["generation"].clone();
        if !status["lastRejection"].is_null() {
            break status;
        }
        assert!(Instant::now() < deadline, "rejection was not reported");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(status["activeRevision"], active_revision);
    assert_eq!(status["degraded"], true);
    assert_eq!(get("/ready").await.status, 200);
    assert_eq!(status["configRejections"], 1);
    assert_eq!(
        status["lastRejection"]["diagnostics"][0]["code"],
        "E_SYNTAX"
    );
    assert!(status["lastRejection"]["atUnixMs"].as_u64().is_some());
    assert!(!status.to_string().contains("secret_external_value"));
    let events = get("/api/v2/events").await.json().to_string();
    assert!(events.contains("configuration_reload"), "{events}");
    let metrics = get("/metrics").await;
    assert!(
        String::from_utf8_lossy(&metrics.body).contains("oxiroute_config_rejections_total 1"),
        "{}",
        metrics.text()
    );

    config.max_connections = Some(103);
    write_config(
        &server.config_path,
        &config.clone().validate().expect("valid process config"),
    );
    let deadline = Instant::now() + process_support::PROCESS_TIMEOUT;
    loop {
        let status = get("/api/v1/generations").await.json()["generation"].clone();
        if status["lastRejection"].is_null() && status["activeRevision"] != active_revision {
            assert_eq!(status["degraded"], false);
            assert_eq!(status["configRejections"], 1);
            break;
        }
        assert!(Instant::now() < deadline, "rejection was not cleared");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    server.shutdown();
}
