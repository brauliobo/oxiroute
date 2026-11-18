use std::{
    fmt::Write as _,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use oxiroute_config::ConfigHistory;
use oxiroute_config_source::ConfigFormat;
use serde_json::{Value, json};

use super::config_source::Daemon;
use crate::http_support::http_request;
use crate::process_support::reserve_tcp_address;
use crate::revision_support::{active_revision, wait_for_new_revision};
use crate::shared::{management_config, public_pool};

const BANNER: &str = "// Edited by hand: this comment must survive every save.\n";

fn history_id(text: &str) -> String {
    openssl::sha::sha256(text.as_bytes())[..6]
        .iter()
        .fold(String::new(), |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        })
}

fn history_directory(daemon: &Daemon) -> PathBuf {
    let mut path = daemon.server.config_path.clone().into_os_string();
    path.push(".history");
    PathBuf::from(path)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

async fn save(daemon: &Daemon, text: &str) -> Value {
    let revision = daemon.disk_revision().await;
    let saved = daemon.put(Some(&revision), text).await;
    assert_eq!(saved.status, 200, "{}", saved.text());
    saved.json()
}

async fn history(daemon: &Daemon) -> Value {
    let response = daemon.get("/api/v1/config/history").await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.json()
}

async fn rollback(
    daemon: &Daemon,
    revision: Option<&str>,
    id: &str,
) -> crate::http_support::HttpResponse {
    daemon
        .send(
            "POST",
            &format!("/api/v1/config/history/{id}/rollback"),
            revision,
            &json!({}),
        )
        .await
}

#[tokio::test(flavor = "current_thread")]
async fn every_save_keeps_the_replaced_text_as_a_private_entry() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let first = format!("{}// one\n", daemon.text);
    let second = format!("{first}// two\n");
    assert_eq!(history(&daemon).await["entries"], json!([]));

    save(&daemon, &first).await;
    save(&daemon, &second).await;

    let listing = history(&daemon).await;
    let entries = listing["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2, "{listing}");
    assert_eq!(entries[0]["id"], history_id(&first));
    assert_eq!(entries[1]["id"], history_id(&daemon.text));
    assert_eq!(entries[0]["bytes"], first.len());
    assert_eq!(listing["current"]["id"], history_id(&second));
    assert!(entries.iter().all(|entry| entry["current"] == false));

    let entry = daemon
        .get(&format!(
            "/api/v1/config/history/{}",
            history_id(&daemon.text)
        ))
        .await;
    assert_eq!(entry.status, 200, "{}", entry.text());
    assert_eq!(entry.json()["text"], daemon.text);
    assert_eq!(entry.json()["format"], "kdl");

    let directory = history_directory(&daemon);
    assert_eq!(mode(&directory), 0o700);
    let files = fs::read_dir(&directory).expect("history files").count();
    assert_eq!(files, 2);
    for file in fs::read_dir(&directory).expect("history files") {
        assert_eq!(mode(&file.expect("file").path()), 0o600);
    }
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn the_history_needs_the_management_token() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    for (method, path) in [
        ("GET", "/api/v1/config/history"),
        ("GET", "/api/v1/config/history/abcdef012345"),
        ("GET", "/api/v1/config/history/diff?from=current&to=current"),
        ("POST", "/api/v1/config/history/abcdef012345/rollback"),
    ] {
        let response = http_request(daemon.address, method, path, &[], &[]).await;
        assert_eq!(response.status, 401, "{method} {path}: {}", response.text());
    }
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_bad_revision_is_rolled_back_and_the_rollback_is_a_new_revision() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let original = active_revision(daemon.address, &daemon.authorization).await;
    let bad = daemon
        .text
        .replace("max_connections #null", "max_connections 77");
    assert_ne!(bad, daemon.text);
    save(&daemon, &bad).await;
    wait_for_new_revision(daemon.address, &daemon.authorization, &original).await;
    let bad_revision = active_revision(daemon.address, &daemon.authorization).await;

    let revision = daemon.disk_revision().await;
    let restored = rollback(&daemon, Some(&revision), &history_id(&daemon.text)).await;

    assert_eq!(restored.status, 200, "{}", restored.text());
    assert_eq!(restored.json()["written"], true);
    assert_eq!(daemon.file(), daemon.text);
    let listing = history(&daemon).await;
    assert_eq!(listing["entries"][0]["id"], history_id(&bad));
    assert_eq!(listing["current"]["id"], history_id(&daemon.text));
    assert_eq!(
        listing["entries"][1]["id"],
        history_id(&daemon.text),
        "the replaced original stays listed"
    );
    assert_eq!(listing["entries"][1]["current"], true);
    let candidate = restored.json()["candidateRevision"]
        .as_str()
        .expect("candidate")
        .to_owned();
    assert_eq!(candidate, original);
    wait_for_new_revision(daemon.address, &daemon.authorization, &bad_revision).await;
    assert_eq!(
        active_revision(daemon.address, &daemon.authorization).await,
        original
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_rollback_follows_the_revision_precondition_and_names_what_it_refuses() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = format!("{}// note\n", daemon.text);
    save(&daemon, &edited).await;
    let revision = daemon.disk_revision().await;
    let id = history_id(&daemon.text);

    let missing = rollback(&daemon, None, &id).await;
    let stale = rollback(&daemon, Some(&"0".repeat(64)), &id).await;
    let unknown = rollback(&daemon, Some(&revision), "000000000000").await;
    let short = rollback(&daemon, Some(&revision), "abc").await;
    let current = rollback(&daemon, Some(&revision), "current").await;

    assert_eq!(missing.status, 428, "{}", missing.text());
    assert_eq!(stale.status, 409, "{}", stale.text());
    assert_eq!(unknown.status, 404, "{}", unknown.text());
    assert_eq!(unknown.json()["error"]["code"], "history_entry_not_found");
    assert_eq!(short.status, 400, "{}", short.text());
    assert_eq!(current.status, 400, "{}", current.text());
    assert_eq!(daemon.file(), edited);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_rollback_is_validated_like_any_apply() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = format!("{}// note\n", daemon.text);
    save(&daemon, &edited).await;
    let entry = history_directory(&daemon)
        .read_dir()
        .expect("history files")
        .next()
        .expect("one entry")
        .expect("entry")
        .path();
    fs::write(&entry, "listeners {\n").expect("corrupt the entry");
    let revision = daemon.disk_revision().await;

    let refused = rollback(&daemon, Some(&revision), &history_id(&daemon.text)).await;

    assert_eq!(refused.status, 422, "{}", refused.text());
    assert_eq!(refused.json()["diagnostics"][0]["code"], "E_SYNTAX");
    assert_eq!(daemon.file(), edited);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_diff_between_two_revisions_lists_settings_and_text() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let changed = daemon
        .text
        .replace("max_connections #null", "max_connections 77");
    save(&daemon, &changed).await;
    let from = history_id(&daemon.text);

    let response = daemon
        .get(&format!(
            "/api/v1/config/history/diff?from={from}&to=current"
        ))
        .await;

    assert_eq!(response.status, 200, "{}", response.text());
    let body = response.json();
    assert_eq!(body["identical"], false);
    assert_eq!(body["diff"]["equal"], false);
    let change = &body["diff"]["changes"][0];
    assert_eq!(change["path"], "/max_connections");
    assert_eq!(change["new"], 77);
    assert!(
        body["textDiff"]
            .as_str()
            .expect("text diff")
            .contains("+max_connections 77")
    );
    let same = daemon
        .get("/api/v1/config/history/diff?from=current&to=current")
        .await;
    assert_eq!(same.json()["identical"], true);
    let incomplete = daemon.get("/api/v1/config/history/diff?from=current").await;
    assert_eq!(incomplete.status, 400);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn the_configured_count_bound_keeps_only_the_newest_versions() {
    let address = reserve_tcp_address();
    let mut config = management_config(address, None);
    config.upstream_pools = vec![public_pool()];
    config.config_history = Some(ConfigHistory {
        max_entries: Some(3),
        max_total_bytes: None,
    });
    let daemon =
        Daemon::start_with("oxiroute.kdl", ConfigFormat::Kdl, BANNER, config, address).await;
    let mut text = daemon.text.clone();
    let mut kept = Vec::new();
    for index in 0..6 {
        kept.push(history_id(&text));
        text = format!("{text}// note {index}\n");
        save(&daemon, &text).await;
    }

    let listing = history(&daemon).await;
    let ids = listing["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_owned())
        .collect::<Vec<_>>();
    kept.reverse();
    assert_eq!(ids, kept[..3]);
    assert_eq!(listing["limits"]["maxEntries"], 3);
    assert_eq!(
        fs::read_dir(history_directory(&daemon))
            .expect("history files")
            .count(),
        3
    );
    daemon.server.shutdown();
}
