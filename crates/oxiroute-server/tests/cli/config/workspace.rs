use std::{fs, net::SocketAddr, process::Command};

use serde_json::{Value, json};

use super::{
    Daemon, ServerProcess, TOKEN, code, http_support, out, path, report, reserve_tcp_address,
};

#[tokio::test(flavor = "multi_thread")]
async fn resource_crud_and_convert_and_files_work_end_to_end() {
    let daemon = Daemon::start().await;

    // 1. CLI config files list & show against running daemon
    let listed = daemon.run(&["--output", "json", "config", "files", "list"]);
    assert_eq!(code(&listed), 0, "{}", report(&listed));
    let listed_json: Value = serde_json::from_str(&out(&listed)).expect("files list JSON");
    assert_eq!(listed_json["rootFormat"], "kdl");
    assert_eq!(listed_json["files"][0]["path"], "oxiroute.kdl");
    assert_eq!(listed_json["files"][0]["kind"], "root");

    let shown = daemon.run(&["config", "files", "show", "oxiroute.kdl"]);
    assert_eq!(code(&shown), 0, "{}", report(&shown));
    assert!(out(&shown).contains("farm"), "{}", out(&shown));

    // 2. CLI config resource CRUD online against daemon (persists back to KDL file)
    let added = daemon.run(&[
        "config",
        "resource",
        "pool-server",
        "set",
        "farm",
        "--name",
        "gamma",
        "--address",
        "127.0.0.1:29",
        "--max-connections",
        "12",
    ]);
    assert_eq!(code(&added), 0, "{}", report(&added));
    let stored = daemon.stored();
    assert!(stored.contains("gamma"), "{stored}");
    assert!(stored.contains("max_connections 12"), "{stored}");

    let got = daemon.run(&[
        "--output", "json", "config", "resource", "get", "pools", "farm",
    ]);
    assert_eq!(code(&got), 0, "{}", report(&got));
    let pool_json: Value = serde_json::from_str(&out(&got)).expect("pool JSON");
    assert_eq!(pool_json["servers"].as_array().map(Vec::len), Some(3));

    let removed = daemon.run(&[
        "config",
        "resource",
        "pool-server",
        "remove",
        "farm",
        "gamma",
    ]);
    assert_eq!(code(&removed), 0, "{}", report(&removed));
    assert!(!daemon.stored().contains("gamma"));

    // 3. Offline convert + offline resource patch across formats
    let yaml_out = daemon.work.path().join("converted.yaml");
    let converted = daemon.run(&[
        "config",
        "convert",
        path(&daemon.server.config_path),
        "--to",
        "yaml",
        "-o",
        path(&yaml_out),
    ]);
    assert_eq!(code(&converted), 0, "{}", report(&converted));
    let patched_offline = daemon.run(&[
        "config",
        "resource",
        "set-global",
        "max_connections=1234",
        "--file",
        path(&yaml_out),
    ]);
    assert_eq!(code(&patched_offline), 0, "{}", report(&patched_offline));
    let yaml_text = fs::read_to_string(&yaml_out).expect("read yaml");
    assert!(yaml_text.contains("max_connections: 1234"), "{yaml_text}");

    // 4. Capabilities & clients & connections CLI commands work
    for sub in [
        &["capabilities"][..],
        &["clients"][..],
        &["connections"][..],
    ] {
        let res = daemon.run(sub);
        assert_eq!(code(&res), 0, "{sub:?}: {}", report(&res));
    }

    daemon.server.shutdown();
}

async fn put_workspace_file(
    address: SocketAddr,
    rel_path: &str,
    text: &str,
    create: bool,
) -> http_support::HttpResponse {
    let auth_header = format!("Bearer {TOKEN}");
    let mut headers = vec![
        ("Authorization", auth_header.as_str()),
        ("Content-Type", "application/json"),
    ];
    let revision = if create {
        None
    } else {
        let meta = http_support::http_request(
            address,
            "GET",
            &format!("/api/v1/config/files/{rel_path}"),
            &[("Authorization", auth_header.as_str())],
            b"",
        )
        .await;
        assert_eq!(meta.status, 200, "{}", meta.text());
        Some(
            meta.json()["fileRevision"]
                .as_str()
                .expect("fileRevision")
                .to_owned(),
        )
    };
    if let Some(rev) = revision.as_deref() {
        headers.push(("If-Config-Revision", rev));
    }
    let body = if create {
        json!({ "text": text, "create": true }).to_string()
    } else {
        json!({ "text": text }).to_string()
    };
    http_support::http_request(
        address,
        "PUT",
        &format!("/api/v1/config/files/{rel_path}"),
        &headers,
        body.as_bytes(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_file_oxi_workspace_api_validates_and_saves_included_child_files() {
    let address = reserve_tcp_address();
    let root_text = format!("version 1\nmanagement\n  bind \"{address}\"\n");
    let mut server = ServerProcess::start_authored("oxiroute.oxi", &root_text, TOKEN);
    server.wait_for_tcp(address).await;
    let token_file = server.token_path.clone().expect("token file");
    let config_dir = server
        .config_path
        .parent()
        .expect("config dir")
        .to_path_buf();

    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_oxiroute"));
        command
            .arg("--endpoint")
            .arg(format!("http://{address}"))
            .arg("--token-file")
            .arg(&token_file)
            .args(args);
        command.output().expect("run oxiroute")
    };

    let created = put_workspace_file(
        address,
        "pools.oxi",
        "upstream_pools\n  -\n    name \"farm\"\n    servers\n      -\n        name \"alpha\"\n        endpoint\n          type \"socket\"\n          address \"127.0.0.1:9\"\n        max_connections 4\n",
        true,
    )
    .await;
    assert_eq!(created.status, 200, "{}", created.text());

    let updated_root = put_workspace_file(
        address,
        "oxiroute.oxi",
        &format!("version 1\nmanagement\n  bind \"{address}\"\ninclude \"pools.oxi\"\n"),
        false,
    )
    .await;
    assert_eq!(updated_root.status, 200, "{}", updated_root.text());

    let listed = run(&["--output", "json", "config", "files", "list"]);
    assert_eq!(code(&listed), 0, "{}", report(&listed));
    let listed_json: Value = serde_json::from_str(&out(&listed)).expect("files list");
    assert_eq!(listed_json["rootFormat"], "oxi");
    let paths: Vec<&str> = listed_json["files"]
        .as_array()
        .expect("files array")
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert!(paths.contains(&"oxiroute.oxi"), "{paths:?}");
    assert!(paths.contains(&"pools.oxi"), "{paths:?}");

    let shown = run(&["config", "files", "show", "pools.oxi"]);
    assert_eq!(code(&shown), 0, "{}", report(&shown));
    assert!(out(&shown).contains("name \"alpha\""), "{}", out(&shown));

    let resp = put_workspace_file(
        address,
        "pools.oxi",
        "upstream_pools\n  -\n    name \"farm\"\n    servers\n      -\n        name \"alpha\"\n        endpoint\n          type \"socket\"\n          address \"127.0.0.1:9\"\n        max_connections 16\n",
        false,
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let updated_child =
        fs::read_to_string(config_dir.join("pools.oxi")).expect("read updated child");
    assert!(
        updated_child.contains("max_connections 16"),
        "{updated_child}"
    );

    server.shutdown();
}
