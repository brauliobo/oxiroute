#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn registered_servers_take_traffic_and_leave_by_lease_or_deregistration() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    let (gamma, _g) = stub("gamma").await;
    assert_eq!(farm.register("render", "alpha", alpha, 2_000).await, 201);
    assert_eq!(farm.register("render", "beta", beta, 2_000).await, 201);
    assert_eq!(farm.register("render", "gamma", gamma, 60_000).await, 201);
    assert_eq!(farm.register("render", "gamma", gamma, 60_000).await, 200);

    assert_eq!(
        farm.served_by("/", 12).await,
        ["alpha", "beta", "gamma"].map(String::from).into()
    );

    let metrics = http_request(farm.management, "GET", "/metrics", &[], &[]).await;
    let metrics = String::from_utf8_lossy(&metrics.body).into_owned();
    assert!(
        metrics.contains("oxiroute_registry_servers{pool=\"render\",state=\"active\"} 3"),
        "registered servers are counted"
    );
    assert!(metrics.contains("oxiroute_registry_operations_total{op=\"put\",result=\"ok\"} 4"));
    assert!(
        metrics.contains(
            "oxiroute_registry_lease_remaining_seconds{pool=\"render\",server=\"gamma\"}"
        )
    );
    assert!(
        metrics.contains("oxiroute_pool_servers{pool=\"render\"} 3"),
        "pool series follow membership"
    );

    // alpha keeps heartbeating, beta stops: only beta must leave within its lease.
    eventually("beta to expire while alpha stays", async || {
        assert_eq!(farm.beat("render", "alpha").await, 200);
        !farm.served_by("/", 9).await.contains("beta")
    })
    .await;
    assert!(farm.served_by("/", 9).await.contains("alpha"));
    let (status, _) = farm
        .api(
            "DELETE",
            "/api/v1/pools/render/servers/gamma",
            TOKEN,
            &Value::Null,
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(
        farm.served_by("/", 9).await,
        ["alpha"].map(String::from).into()
    );
    assert_eq!(
        farm.beat("render", "beta").await,
        404,
        "an expired worker is told to register again"
    );
    server.shutdown();
}

#[tokio::test]
pub(super) async fn a_second_pool_registers_with_the_same_model_and_a_scoped_token() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let (render, _r) = stub("gpu-1").await;
    let body = json!({"address": voice.to_string()});

    let (status, _) = farm
        .api(
            "PUT",
            "/api/v1/pools/voice/servers/tts-1",
            &farm.voice_secret,
            &body,
        )
        .await;
    assert_eq!(status, 201);
    let (denied, error) = farm
        .api(
            "PUT",
            "/api/v1/pools/render/servers/tts-1",
            &farm.voice_secret,
            &body,
        )
        .await;
    assert_eq!(
        (denied, error["error"]["code"].as_str()),
        (403, Some("scope_denied"))
    );
    let (listing, _) = farm
        .api(
            "GET",
            "/api/v1/registrations",
            &farm.voice_secret,
            &Value::Null,
        )
        .await;
    assert_eq!(listing, 403, "a register-only token cannot read");
    let (nobody, _) = farm
        .api(
            "PUT",
            "/api/v1/pools/voice/servers/tts-2",
            "not-a-token",
            &body,
        )
        .await;
    assert_eq!(nobody, 401);
    assert_eq!(farm.register("render", "gpu-1", render, 60_000).await, 201);

    assert_eq!(
        farm.served_by("/voice/x", 4).await,
        ["tts-1"].map(String::from).into()
    );
    assert_eq!(
        farm.served_by("/", 4).await,
        ["gpu-1"].map(String::from).into()
    );
    let (status, topology) = farm
        .api("GET", "/api/v1/topology", TOKEN, &Value::Null)
        .await;
    assert_eq!(status, 200, "{topology}");
    let node = topology["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["name"] == "tts-1"))
        .expect("the registered server is a topology node");
    assert_eq!(node["attributes"]["origin"], "dynamic");
    assert!(
        topology["edges"]
            .as_array()
            .is_some_and(|edges| edges.iter().any(|edge| edge["target"] == node["id"])),
        "a pool edge reaches it"
    );
    for path in [
        "/api/v1/monitoring",
        "/api/v1/servers",
        "/api/v1/pools",
        "/api/v1/status",
    ] {
        let (status, _) = farm.api("GET", path, TOKEN, &Value::Null).await;
        assert_eq!(status, 200, "{path} copes with registered servers");
    }
    let (status, listing) = farm
        .api("GET", "/api/v1/pools/voice/servers", TOKEN, &Value::Null)
        .await;
    assert_eq!(
        (status, listing["servers"][0]["origin"].as_str()),
        (200, Some("dynamic"))
    );
    server.shutdown();
}

#[tokio::test]
pub(super) async fn registrations_and_operator_drain_survive_generation_reload_and_restart() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    farm.register("render", "alpha", alpha, 60_000).await;
    farm.register("render", "beta", beta, 60_000).await;
    let (status, _) = farm
        .api(
            "POST",
            "/api/v1/pools/render/servers/beta/state",
            TOKEN,
            &json!({"state": "drain"}),
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into()
    );

    let (_, generations) = farm
        .api("GET", "/api/v1/generations", TOKEN, &Value::Null)
        .await;
    let (status, _) = farm
        .api(
            "PUT",
            "/api/v1/servers/weight",
            TOKEN,
            &json!({
                "targets": [{"pool": "render", "server": "alpha"}],
                "expectedActiveRevision": generations["generation"]["activeRevision"],
                "weight": 5,
            }),
        )
        .await;
    assert_eq!(status, 200);

    write_config(&server.config_path, &farm.config(Some(500)));
    eventually("the changed file to become a new generation", async || {
        let metrics = http_request(farm.management, "GET", "/metrics", &[], &[]).await;
        String::from_utf8_lossy(&metrics.body).contains("oxiroute_generation_activations_total 2")
    })
    .await;
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into(),
        "drain survives the reload"
    );
    assert_eq!(
        farm.beat("render", "beta").await,
        200,
        "the lease survives the reload"
    );
    let (_, servers) = farm
        .api("GET", "/api/v1/servers", TOKEN, &Value::Null)
        .await;
    let alpha = servers["servers"]
        .as_array()
        .and_then(|all| all.iter().find(|entry| entry["server"]["name"] == "alpha"))
        .expect("alpha is listed");
    assert_eq!(
        (
            alpha["server"]["origin"].as_str(),
            alpha["server"]["weight"].as_u64()
        ),
        (Some("dynamic"), Some(5)),
        "the operator's live weight survives the reload"
    );

    server.shutdown_gracefully();
    let restarted = farm.start().await;
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into(),
        "drain survives a restart"
    );
    let (_, listing) = farm
        .api("GET", "/api/v1/registrations", TOKEN, &Value::Null)
        .await;
    assert_eq!(listing["registrations"].as_array().map(Vec::len), Some(2));
    restarted.shutdown();
}

#[tokio::test]
pub(super) async fn group_sockets_split_status_from_registration_without_tokens() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let body = json!({"address": voice.to_string()});
    for role in ["read", "register"] {
        let mode = fs::metadata(farm.socket(role))
            .expect("socket")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o660, "{role} socket mode");
    }

    let (status, _) = farm
        .over_socket(
            "register",
            "PUT",
            "/api/v1/pools/voice/servers/tts-1",
            &body,
        )
        .await;
    assert_eq!(status, 201, "a socket client needs no token");
    let (status, error) = farm
        .over_socket(
            "register",
            "PUT",
            "/api/v1/pools/render/servers/tts-1",
            &body,
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (403, Some("scope_denied"))
    );
    let (status, _) = farm
        .over_socket("register", "GET", "/api/v1/registrations", &Value::Null)
        .await;
    assert_eq!(status, 403, "the register socket cannot read");
    let (status, _) = farm
        .over_socket("register", "GET", "/api/v1/status", &Value::Null)
        .await;
    assert_eq!(status, 404, "only registry routes are served");

    let (status, listing) = farm
        .over_socket("read", "GET", "/api/v1/registrations", &Value::Null)
        .await;
    assert_eq!(status, 200);
    assert_eq!(listing["registrations"][0]["registration"]["name"], "tts-1");
    let (status, _) = farm
        .over_socket(
            "read",
            "DELETE",
            "/api/v1/pools/voice/servers/tts-1",
            &Value::Null,
        )
        .await;
    assert_eq!(status, 403, "the read socket cannot change anything");
    let (status, _) = farm
        .over_socket("read", "GET", "/ready", &Value::Null)
        .await;
    assert_eq!(status, 200);
    server.shutdown();
}

#[tokio::test]
pub(super) async fn the_read_socket_serves_the_status_page_with_registry_leases() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    assert_eq!(farm.register("voice", "tts-1", voice, 2_000).await, 201);

    let (status, page) = farm
        .over_socket("read", "GET", "/status.json", &Value::Null)
        .await;
    assert_eq!(status, 200);
    assert_eq!(page["ready"], true);
    assert_eq!(page["registry"]["leases"], 1);
    assert_eq!(page["registry"]["degraded"], false);
    let pool = page["pools"]
        .as_array()
        .and_then(|pools| pools.iter().find(|pool| pool["name"] == "voice"))
        .expect("voice pool");
    assert_eq!(pool["total"], 1);
    let (status, _) = farm
        .over_socket("register", "GET", "/status.json", &Value::Null)
        .await;
    assert_eq!(status, 404, "only the read socket serves the status page");
    server.shutdown();
}

#[tokio::test]
pub(super) async fn the_lan_endpoint_needs_a_scoped_token_and_a_peer_in_range() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let body = json!({"address": voice.to_string()});
    let path = "/api/v1/pools/voice/servers/tts-1";

    assert_eq!(farm.over_lan("PUT", path, "", &body).await.0, 401);
    assert_eq!(
        farm.over_lan("PUT", path, TOKEN, &body).await.0,
        401,
        "the root token is not accepted here"
    );
    assert_eq!(
        farm.over_lan("PUT", path, &farm.voice_secret, &body)
            .await
            .0,
        201
    );
    assert_eq!(
        farm.over_lan(
            "POST",
            &format!("{path}/heartbeat"),
            &farm.voice_secret,
            &Value::Null
        )
        .await
        .0,
        200
    );
    let (status, _) = farm
        .over_lan(
            "GET",
            "/api/v1/registrations",
            &farm.voice_secret,
            &Value::Null,
        )
        .await;
    assert_eq!(status, 403);
    assert_eq!(
        farm.over_lan(
            "PUT",
            "/api/v1/pools/render/servers/x",
            &farm.voice_secret,
            &body
        )
        .await
        .0,
        403
    );
    server.shutdown();

    let mut outside = Farm::new();
    outside.lan_cidrs = vec!["192.0.2.0/8".into()];
    let server = outside.start().await;
    let (status, error) = outside
        .over_lan("PUT", path, &outside.voice_secret, &body)
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (403, Some("peer_not_allowed"))
    );
    server.shutdown();
}

#[tokio::test]
pub(super) async fn draining_a_registered_server_through_the_server_api_survives_restart() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    farm.register("render", "alpha", alpha, 60_000).await;
    farm.register("render", "beta", beta, 60_000).await;
    let (_, generations) = farm
        .api("GET", "/api/v1/generations", TOKEN, &Value::Null)
        .await;
    let (status, _) = farm
        .api(
            "POST",
            "/api/v1/servers/administrative-state",
            TOKEN,
            &json!({
                "targets": [{"pool": "render", "server": "beta"}],
                "expectedActiveRevision": generations["generation"]["activeRevision"],
                "state": "drain",
            }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into()
    );

    server.shutdown_gracefully();
    let restarted = farm.start().await;
    assert_eq!(
        farm.served_by("/", 8).await,
        ["alpha"].map(String::from).into(),
        "the drain outlives a restart"
    );
    restarted.shutdown();
}

pub(super) fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[tokio::test]
pub(super) async fn the_cli_registers_lists_and_removes_servers_over_group_sockets() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let address = voice.to_string();
    let (flag, register) = farm.socket_arg("register");
    let (_, read) = farm.socket_arg("read");

    let registered = farm
        .cli(
            (flag, &register),
            &[
                "server",
                "register",
                "--pool",
                "voice",
                "tts-1",
                "--address",
                &address,
                "--weight",
                "3",
                "--ttl",
                "30s",
                "--label",
                "gpu=0",
            ],
        )
        .await;
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );

    let listed = farm
        .cli((flag, &read), &["server", "list", "--pool", "voice"])
        .await;
    let listing = stdout_json(&listed);
    let entry = &listing["servers"][0];
    assert_eq!(
        (
            entry["name"].as_str(),
            entry["origin"].as_str(),
            entry["weight"].as_u64()
        ),
        (Some("tts-1"), Some("dynamic"), Some(3))
    );
    assert_eq!(entry["registration"]["ttlMs"], 30_000);
    let none = farm
        .cli(
            (flag, &read),
            &["server", "list", "--pool", "voice", "--origin", "static"],
        )
        .await;
    assert_eq!(
        stdout_json(&none)["servers"].as_array().map(Vec::len),
        Some(0)
    );

    let removed = farm
        .cli(
            (flag, &register),
            &["server", "deregister", "--pool", "voice", "tts-1"],
        )
        .await;
    assert!(removed.status.success());
    let again = farm
        .cli(
            (flag, &register),
            &["server", "deregister", "--pool", "voice", "tts-1"],
        )
        .await;
    assert_eq!(again.status.code(), Some(6));
    let tolerated = farm
        .cli(
            (flag, &register),
            &[
                "server",
                "deregister",
                "--pool",
                "voice",
                "tts-1",
                "--missing-ok",
            ],
        )
        .await;
    assert!(tolerated.status.success());
    server.shutdown();
}

#[tokio::test]
pub(super) async fn a_heartbeating_cli_keeps_its_lease_and_leaves_cleanly_on_sigterm() {
    let farm = Farm::new();
    let server = farm.start().await;
    let (voice, _v) = stub("tts-1").await;
    let address = voice.to_string();
    let register = farm.socket("register").display().to_string();
    let mut agent = Command::new(env!("CARGO_BIN_EXE_oxiroute"))
        .args([
            "--socket",
            &register,
            "server",
            "register",
            "--pool",
            "voice",
            "tts-1",
            "--address",
            &address,
            "--ttl",
            "2s",
            "--heartbeat",
        ])
        .stdout(Stdio::null())
        .spawn()
        .expect("agent");
    eventually("the agent to register", async || {
        farm.over_socket("read", "GET", "/api/v1/pools/voice/servers", &Value::Null)
            .await
            .1["servers"]
            .as_array()
            .is_some_and(|servers| servers.len() == 1)
    })
    .await;

    tokio::time::sleep(Duration::from_secs(5)).await;
    let (_, listing) = farm
        .over_socket("read", "GET", "/api/v1/pools/voice/servers", &Value::Null)
        .await;
    assert_eq!(
        listing["servers"].as_array().map(Vec::len),
        Some(1),
        "heartbeats outlive the 2s lease"
    );

    assert!(
        Command::new("kill")
            .args(["-TERM", &agent.id().to_string()])
            .status()
            .expect("kill")
            .success()
    );
    let exit = tokio::task::spawn_blocking(move || agent.wait().expect("agent exit"))
        .await
        .expect("wait");
    assert!(exit.success(), "{exit:?}");
    let (_, listing) = farm
        .over_socket("read", "GET", "/api/v1/pools/voice/servers", &Value::Null)
        .await;
    assert_eq!(
        listing["servers"].as_array().map(Vec::len),
        Some(0),
        "SIGTERM deregisters"
    );
    server.shutdown();
}

#[tokio::test]
pub(super) async fn a_supervised_worker_handover_keeps_registrations_and_sockets() {
    let farm = Farm::new();
    let server = farm.start_supervised().await;
    let (alpha, _a) = stub("alpha").await;
    let (beta, _b) = stub("beta").await;
    let (gamma, _g) = stub("gamma").await;
    assert_eq!(farm.register("render", "alpha", alpha, 60_000).await, 201);
    let body = json!({"address": beta.to_string(), "ttlMs": 60_000});
    let (status, _) = farm
        .over_socket("register", "PUT", "/api/v1/pools/voice/servers/beta", &body)
        .await;
    assert_eq!(status, 201);
    let (_, before) = farm
        .api("GET", "/api/v1/generations", TOKEN, &Value::Null)
        .await;

    write_config(&server.config_path, &farm.config(Some(500)));
    eventually("a new worker to take over", async || {
        let (_, now) = farm
            .api("GET", "/api/v1/generations", TOKEN, &Value::Null)
            .await;
        now["generation"]["activeRevision"] != before["generation"]["activeRevision"]
    })
    .await;

    eventually(
        "the new worker to serve the registered server",
        async || farm.served_by("/", 4).await == ["alpha"].map(String::from).into(),
    )
    .await;
    assert_eq!(
        farm.beat("render", "alpha").await,
        200,
        "the lease survives the handover"
    );
    let body = json!({"address": gamma.to_string(), "ttlMs": 60_000});
    let (status, _) = farm
        .over_socket(
            "register",
            "PUT",
            "/api/v1/pools/voice/servers/gamma",
            &body,
        )
        .await;
    assert_eq!(status, 201, "the register socket belongs to the new worker");
    let (_, listing) = farm
        .over_socket("read", "GET", "/api/v1/registrations", &Value::Null)
        .await;
    assert_eq!(listing["registrations"].as_array().map(Vec::len), Some(3));
    server.shutdown();
}

#[tokio::test]
pub(super) async fn the_cli_registers_over_the_lan_with_a_scoped_token_file() {
    let farm = Farm::new();
    let server = farm.start().await;
    let secret = farm.state.path().join("voice.secret");
    fs::write(&secret, &farm.voice_secret).expect("secret file");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).expect("mode");
    let lan = format!("http://{}", farm.test);
    let secret = secret.display().to_string();
    let (second, _s) = stub("tts-2").await;
    let second = second.to_string();
    let over_lan = async |pool: &str| {
        farm.cli(
            ("--endpoint", &lan),
            &[
                "--token-file",
                &secret,
                "server",
                "register",
                "--pool",
                pool,
                "tts-2",
                "--address",
                &second,
            ],
        )
        .await
    };
    let joined = over_lan("voice").await;
    assert!(
        joined.status.success(),
        "a scoped token registers over the LAN endpoint: {}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert_eq!(
        over_lan("render").await.status.code(),
        Some(5),
        "another pool is refused"
    );

    server.shutdown();
}
