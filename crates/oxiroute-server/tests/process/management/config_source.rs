use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use oxiroute_config::{ConfigDraft, RtmpTokenPolicy, RtmpTokenSource};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::http_support::{HttpResponse, http_request};
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::revision_support::{active_revision, wait_for_new_revision};
use crate::shared::{TOKEN, management_config, public_pool, recording_candidate};

const BANNER: &str = "// Edited by hand: this comment must survive every save.\n";

pub(super) struct Daemon {
    pub(super) server: ServerProcess,
    pub(super) address: SocketAddr,
    pub(super) authorization: String,
    pub(super) text: String,
}

impl Daemon {
    pub(super) async fn start(file_name: &str, format: ConfigFormat, banner: &str) -> Self {
        let address = reserve_tcp_address();
        let mut config = management_config(address, None);
        config.upstream_pools = vec![public_pool()];
        Self::start_with(file_name, format, banner, config, address).await
    }

    pub(super) async fn start_with(
        file_name: &str,
        format: ConfigFormat,
        banner: &str,
        config: ConfigDraft,
        address: SocketAddr,
    ) -> Self {
        let validated = config.validate().expect("valid process config");
        let text = format!(
            "{banner}{}",
            render_config(format, &validated).expect("render process config")
        );
        let mut server = ServerProcess::start_authored(file_name, &text, TOKEN);
        server.wait_for_tcp(address).await;
        Self {
            server,
            address,
            authorization: format!("Bearer {TOKEN}"),
            text,
        }
    }

    pub(super) async fn get(&self, path: &str) -> HttpResponse {
        http_request(
            self.address,
            "GET",
            path,
            &[("Authorization", &self.authorization)],
            &[],
        )
        .await
    }

    pub(super) async fn send(
        &self,
        method: &str,
        path: &str,
        revision: Option<&str>,
        body: &Value,
    ) -> HttpResponse {
        let mut headers = vec![
            ("Authorization", self.authorization.as_str()),
            ("Content-Type", "application/json"),
        ];
        if let Some(revision) = revision {
            headers.push(("If-Config-Revision", revision));
        }
        http_request(
            self.address,
            method,
            path,
            &headers,
            body.to_string().as_bytes(),
        )
        .await
    }

    pub(super) async fn put(&self, revision: Option<&str>, text: &str) -> HttpResponse {
        self.send(
            "PUT",
            "/api/v1/config/source",
            revision,
            &json!({ "text": text }),
        )
        .await
    }

    pub(super) async fn validate(&self, body: &Value) -> HttpResponse {
        self.send("POST", "/api/v1/config/source/validate", None, body)
            .await
    }

    pub(super) async fn disk_revision(&self) -> String {
        let source = self.get("/api/v1/config/source").await;
        assert_eq!(source.status, 200, "{}", source.text());
        source.json()["diskRevision"]
            .as_str()
            .expect("disk revision")
            .to_owned()
    }

    pub(super) fn file(&self) -> String {
        fs::read_to_string(&self.server.config_path).expect("configuration file")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_authored_text_comes_back_byte_for_byte_with_its_revisions() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let source = daemon.get("/api/v1/config/source").await;

    assert_eq!(source.status, 200, "{}", source.text());
    let body = source.json();
    assert_eq!(body["text"], daemon.text);
    assert_eq!(body["format"], "kdl");
    assert_eq!(body["view"], "disk");
    assert_eq!(body["authored"], true);
    assert_eq!(body["valid"], true);
    assert_eq!(body["redaction"]["redacted"], 0);
    assert_eq!(body["redaction"]["withheld"], false);
    assert_eq!(body["configWritable"], true);
    assert_eq!(body["writeBlock"], Value::Null);
    assert_eq!(body["diskRevision"].as_str().map(str::len), Some(64));
    assert_eq!(body["candidateRevision"], body["activeRevision"]);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_put_stores_the_authored_bytes_and_keeps_the_file_mode() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    fs::set_permissions(
        &daemon.server.config_path,
        fs::Permissions::from_mode(0o640),
    )
    .expect("loosen the file mode");
    let revision = daemon.disk_revision().await;
    let edited = format!("{}// A second note.\n", daemon.text);

    let saved = daemon.put(Some(&revision), &edited).await;

    assert_eq!(saved.status, 200, "{}", saved.text());
    let body = saved.json();
    assert_eq!(body["written"], true);
    assert_eq!(daemon.file(), edited);
    let mode = fs::metadata(&daemon.server.config_path)
        .expect("file metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o640);
    assert_eq!(daemon.disk_revision().await, body["diskRevision"]);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_stale_or_missing_revision_never_writes() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = format!("{}// edit\n", daemon.text);

    let stale = daemon.put(Some(&"0".repeat(64)), &edited).await;
    let missing = daemon.put(None, &edited).await;

    assert_eq!(stale.status, 409, "{}", stale.text());
    assert_eq!(stale.json()["outcome"], "conflict");
    assert_eq!(missing.status, 428, "{}", missing.text());
    assert_eq!(daemon.file(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn putting_the_stored_text_again_writes_nothing() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let before = fs::metadata(&daemon.server.config_path).expect("file metadata");
    let revision = daemon.disk_revision().await;

    let saved = daemon.put(Some(&revision), &daemon.text).await;

    assert_eq!(saved.status, 200, "{}", saved.text());
    assert_eq!(saved.json()["written"], false);
    assert_eq!(saved.json()["outcome"], "unchanged_active");
    let after = fs::metadata(&daemon.server.config_path).expect("file metadata");
    assert_eq!(
        (before.ino(), before.mtime_nsec()),
        (after.ino(), after.mtime_nsec())
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_text_is_refused_with_its_diagnostics_and_the_file_stays() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let revision = daemon.disk_revision().await;
    let broken = format!("{}\nlisteners {{\n", daemon.text);

    let validated = daemon.validate(&json!({ "text": broken })).await;
    let saved = daemon.put(Some(&revision), &broken).await;

    for response in [&validated, &saved] {
        assert_eq!(response.status, 422, "{}", response.text());
        let diagnostic = &response.json()["diagnostics"][0];
        assert_eq!(diagnostic["code"], "E_SYNTAX");
        assert!(diagnostic["line"].as_u64().is_some(), "{diagnostic}");
    }
    assert_eq!(daemon.file(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn other_formats_validate_but_only_the_files_format_is_stored() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let revision = daemon.disk_revision().await;
    let mut config = management_config(daemon.address, None);
    config.upstream_pools = vec![public_pool()];
    let validated = config.validate().expect("valid config");
    let hocon = render_config(ConfigFormat::Hocon, &validated).expect("hocon");

    let checked = daemon
        .validate(&json!({ "text": hocon, "format": "json" }))
        .await;
    let refused = daemon
        .send(
            "PUT",
            "/api/v1/config/source",
            Some(&revision),
            &json!({ "text": hocon, "format": "json" }),
        )
        .await;

    assert_eq!(checked.status, 200, "{}", checked.text());
    assert_eq!(checked.json()["format"], "hocon");
    assert_eq!(checked.json()["matchesDiskFormat"], false);
    assert_eq!(refused.status, 422, "{}", refused.text());
    assert_eq!(
        refused.json()["diagnostics"][0]["code"],
        "E_FORMAT_MISMATCH"
    );
    assert_eq!(daemon.file(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn lua_text_is_stored_exactly_and_the_daemon_activates_it() {
    let banner = "-- Edited by hand.\nlocal note = \"kept\"\n";
    let daemon = Daemon::start("oxiroute.lua", ConfigFormat::Lua, banner).await;
    let revision = daemon.disk_revision().await;
    let original = active_revision(daemon.address, &daemon.authorization).await;
    let edited = daemon
        .text
        .replace("local note = \"kept\"", "local note = \"kept twice\"");

    let saved = daemon.put(Some(&revision), &edited).await;

    assert_eq!(saved.status, 200, "{}", saved.text());
    assert_eq!(daemon.file(), edited);
    assert_eq!(saved.json()["outcome"], "unchanged_active");
    assert_eq!(saved.json()["candidateRevision"], original);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_changed_setting_becomes_the_active_revision() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let revision = daemon.disk_revision().await;
    let original = active_revision(daemon.address, &daemon.authorization).await;
    let edited = daemon
        .text
        .replace("max_connections #null", "max_connections 77");
    assert_ne!(
        edited, daemon.text,
        "the canonical text sets max_connections"
    );

    let saved = daemon.put(Some(&revision), &edited).await;

    assert_eq!(saved.status, 200, "{}", saved.text());
    assert_eq!(saved.json()["outcome"], "saved_pending_activation");
    let candidate = saved.json()["candidateRevision"]
        .as_str()
        .expect("candidate")
        .to_owned();
    assert_ne!(candidate, original);
    wait_for_new_revision(daemon.address, &daemon.authorization, &original).await;
    assert_eq!(
        active_revision(daemon.address, &daemon.authorization).await,
        candidate
    );
    assert_eq!(daemon.file(), edited);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn the_active_view_renders_the_running_configuration() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let active = daemon.get("/api/v1/config/source?view=active").await;
    let invalid = daemon.get("/api/v1/config/source?view=other").await;

    assert_eq!(active.status, 200, "{}", active.text());
    let body = active.json();
    assert_eq!(body["view"], "active");
    assert_eq!(body["authored"], false);
    let text = body["text"].as_str().expect("active text");
    assert!(
        !text.contains("must survive"),
        "comments are not part of the active view"
    );
    assert!(text.contains("origin-a"));
    assert_eq!(invalid.status, 400);
    daemon.server.shutdown();
}

fn with_rtmp_secrets(address: SocketAddr, root: &Path) -> ConfigDraft {
    let mut config = recording_candidate(&management_config(address, None), root);
    config.rtmp_services[0].outbound_policy.deny_private = false;
    let application = &mut config.rtmp_services[0].applications[0];
    application.recorders.clear();
    application.publish.token = Some(RtmpTokenPolicy {
        source: RtmpTokenSource::StreamQuery,
        parameter: "token".into(),
        secret: "publish-secret-not-real".into(),
    });
    application.callbacks.on_publish =
        Some("http://127.0.0.1:9/publish?key=callback-key-not-real".into());
    config
}

#[tokio::test(flavor = "current_thread")]
async fn inline_rtmp_secrets_are_redacted_and_restored_on_save() {
    let root = TempDir::new().expect("recording root");
    let address = reserve_tcp_address();
    let config = with_rtmp_secrets(address, root.path());
    let daemon =
        Daemon::start_with("oxiroute.kdl", ConfigFormat::Kdl, BANNER, config, address).await;
    let source = daemon.get("/api/v1/config/source").await;
    let body = source.json();
    let text = body["text"].as_str().expect("redacted text").to_owned();

    assert_eq!(source.status, 200, "{}", source.text());
    for secret in ["publish-secret-not-real", "callback-key-not-real"] {
        assert!(!source.text().contains(secret), "{secret} leaked");
    }
    assert!(text.contains("<redacted:/rtmp_services/"), "{text}");
    assert_eq!(body["redaction"]["redacted"], 2);

    let edited = format!("{text}// kept\n");
    let saved = daemon.put(body["diskRevision"].as_str(), &edited).await;

    assert_eq!(saved.status, 200, "{}", saved.text());
    let stored = daemon.file();
    assert!(stored.contains("publish-secret-not-real"));
    assert!(stored.contains("callback-key-not-real"));
    assert!(stored.ends_with("// kept\n"));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn an_unknown_placeholder_is_refused() {
    let root = TempDir::new().expect("recording root");
    let address = reserve_tcp_address();
    let config = with_rtmp_secrets(address, root.path());
    let daemon =
        Daemon::start_with("oxiroute.kdl", ConfigFormat::Kdl, BANNER, config, address).await;
    let revision = daemon.disk_revision().await;
    let text = daemon.get("/api/v1/config/source").await.json()["text"]
        .as_str()
        .expect("text")
        .replace("rtmp_services/recording-wire", "rtmp_services/missing");

    let refused = daemon.put(Some(&revision), &text).await;

    assert_eq!(refused.status, 422, "{}", refused.text());
    assert_eq!(
        refused.json()["error"]["code"],
        "redacted_secret_unresolved"
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn an_unwritable_directory_is_reported_and_named_when_a_save_fails() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let directory = daemon
        .server
        .config_path
        .parent()
        .expect("directory")
        .to_owned();
    let revision = daemon.disk_revision().await;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).expect("lock directory");

    let status = daemon.get("/api/v1/status").await.json();
    let source = daemon.get("/api/v1/config/source").await.json();
    let saved = daemon
        .put(Some(&revision), &format!("{}// x\n", daemon.text))
        .await;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("unlock directory");

    assert_eq!(status["configWritable"], false);
    assert_eq!(source["configWritable"], false);
    assert_eq!(source["writeBlock"], "permission_denied");
    assert_eq!(saved.status, 500, "{}", saved.text());
    assert_eq!(
        saved.json()["diagnostics"][0]["code"],
        "E_CONFIG_NOT_WRITABLE"
    );
    assert_eq!(daemon.file(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_a_writable_configuration() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    assert_eq!(
        daemon.get("/api/v1/status").await.json()["configWritable"],
        true
    );
    daemon.server.shutdown();
}

fn change<'a>(comparison: &'a Value, path: &str) -> &'a Value {
    comparison["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .find(|change| change["path"] == path)
        .unwrap_or_else(|| panic!("no change at {path}: {comparison}"))
}

#[tokio::test(flavor = "current_thread")]
async fn validation_returns_the_path_aware_changes_against_disk_and_active() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = daemon
        .text
        .replace("max_connections #null", "max_connections 77");
    let same = daemon
        .validate(&json!({ "text": daemon.text }))
        .await
        .json();

    let checked = daemon.validate(&json!({ "text": edited })).await;

    assert_eq!(checked.status, 200, "{}", checked.text());
    let diff = &checked.json()["diff"];
    for base in ["disk", "active"] {
        assert_eq!(diff[base]["equal"], false);
        let server = change(
            &diff[base],
            "/upstream_pools/public/servers/origin-a/max_connections",
        );
        assert_eq!(server["kind"], "changed");
        assert_eq!(server["old"], Value::Null);
        assert_eq!(server["new"], 77);
        assert_eq!(change(&diff[base], "/max_connections")["new"], 77);
    }
    assert_eq!(diff["pending"]["equal"], true);
    assert_eq!(same["diff"]["disk"]["equal"], true);
    assert_eq!(same["diff"]["active"]["equal"], true);
    assert_eq!(same["textDiff"], Value::Null);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn added_and_removed_list_items_are_named() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = daemon.text.replace("origin-a", "origin-b");

    let checked = daemon.validate(&json!({ "text": edited })).await.json();

    let diff = &checked["diff"]["active"];
    assert_eq!(
        change(diff, "/upstream_pools/public/servers/origin-a")["kind"],
        "removed"
    );
    assert_eq!(
        change(diff, "/upstream_pools/public/servers/origin-b")["kind"],
        "added"
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn the_text_diff_is_unified_and_only_present_when_asked_for() {
    let daemon = Daemon::start("oxiroute.kdl", ConfigFormat::Kdl, BANNER).await;
    let edited = format!("{}// appended\n", daemon.text);

    let asked = daemon
        .validate(&json!({ "text": edited, "textDiff": true }))
        .await
        .json();
    let not_asked = daemon.validate(&json!({ "text": edited })).await.json();

    let text_diff = asked["textDiff"].as_str().expect("text diff");
    assert!(
        text_diff.starts_with("--- disk\n+++ candidate\n@@ "),
        "{text_diff}"
    );
    assert!(text_diff.contains("+// appended\n"), "{text_diff}");
    assert_eq!(asked["diff"]["disk"]["equal"], true);
    assert_eq!(not_asked["textDiff"], Value::Null);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "current_thread")]
async fn a_changed_secret_shows_as_changed_without_either_value() {
    let root = TempDir::new().expect("recording root");
    let address = reserve_tcp_address();
    let config = with_rtmp_secrets(address, root.path());
    let daemon =
        Daemon::start_with("oxiroute.kdl", ConfigFormat::Kdl, BANNER, config, address).await;
    let text = daemon.get("/api/v1/config/source").await.json()["text"]
        .as_str()
        .expect("text")
        .to_owned();
    let placeholder =
        "<redacted:/rtmp_services/recording-wire/applications/live/publish/token/secret>";
    assert!(text.contains(placeholder), "{text}");
    let rotated = text.replace(placeholder, "rotated-secret-not-real");

    let checked = daemon
        .validate(&json!({ "text": rotated, "textDiff": true }))
        .await;

    assert_eq!(checked.status, 200, "{}", checked.text());
    for secret in [
        "publish-secret-not-real",
        "rotated-secret-not-real",
        "callback-key-not-real",
    ] {
        let text = checked.text();
        let at = text.find(secret);
        assert!(
            at.is_none(),
            "{secret} leaked near {:?}",
            at.map(|at| &text[at.saturating_sub(200)..(at + 100).min(text.len())])
        );
    }
    let body = checked.json();
    let secret = change(
        &body["diff"]["disk"],
        "/rtmp_services/recording-wire/applications/live/publish/token/secret",
    );
    assert_eq!(secret["kind"], "changed");
    assert_eq!(secret["old"], "<redacted>");
    assert_eq!(secret["new"], "<redacted>");
    daemon.server.shutdown();
}
