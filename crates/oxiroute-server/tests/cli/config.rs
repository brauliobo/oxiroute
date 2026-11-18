//! `oxiroute config` against a real daemon running a KDL file written by hand, and offline.

#[path = "../support/config.rs"]
mod config_support;
#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[allow(dead_code)]
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/revision.rs"]
mod revision_support;

use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use oxiroute_config::{ConfigDraft, Management, UpstreamPool};
use oxiroute_config_source::{ConfigFormat, render_config};
use serde_json::{Value, json};
use tempfile::TempDir;

use config_support::empty_config;
use process_support::{ServerProcess, reserve_tcp_address};
use revision_support::active_revision;

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const BANNER: &str = "// Edited by hand: this comment must survive every apply.\n";

fn draft(management: SocketAddr) -> ConfigDraft {
    let pool: UpstreamPool = serde_json::from_value(json!({
        "name": "farm",
        "servers": [
            { "name": "alpha", "endpoint": { "type": "socket", "address": "127.0.0.1:9" }, "max_connections": 4 },
            { "name": "beta", "endpoint": { "type": "socket", "address": "127.0.0.1:19" }, "max_connections": 4 },
        ],
    }))
    .expect("farm pool");
    ConfigDraft {
        management: Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        }),
        upstream_pools: vec![pool],
        ..empty_config()
    }
}

fn rendered(draft: ConfigDraft, format: ConfigFormat) -> String {
    let validated = draft.validate().expect("valid config");
    render_config(format, &validated).expect("rendered config")
}

struct Daemon {
    server: ServerProcess,
    address: SocketAddr,
    token_file: PathBuf,
    text: String,
    work: TempDir,
}

impl Daemon {
    async fn start() -> Self {
        let address = reserve_tcp_address();
        let text = format!("{BANNER}{}", rendered(draft(address), ConfigFormat::Kdl));
        let mut server = ServerProcess::start_authored("oxiroute.kdl", &text, TOKEN);
        server.wait_for_tcp(address).await;
        let token_file = server.token_path.clone().expect("token file");
        Self {
            server,
            address,
            token_file,
            text,
            work: TempDir::new().expect("work directory"),
        }
    }

    fn run(&self, words: &[&str]) -> Output {
        self.command(words).output().expect("oxiroute CLI")
    }

    fn command(&self, words: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_oxiroute"));
        command
            .env_remove("OXIROUTE_ENDPOINT")
            .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .args([
                "--endpoint",
                &format!("http://{}", self.address),
                "--token-file",
            ])
            .arg(&self.token_file)
            .args(words);
        command
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.work.path().join(name);
        fs::write(&path, text).expect("write input file");
        path
    }

    fn stored(&self) -> String {
        fs::read_to_string(&self.server.config_path).expect("stored configuration")
    }

    fn editor(&self, name: &str, script: &str) -> PathBuf {
        let path = self.work.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write editor");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("editor mode");
        path
    }
}

fn authorization() -> String {
    format!("Bearer {TOKEN}")
}

fn path(file: &Path) -> &str {
    file.to_str().expect("UTF-8 path")
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn report(output: &Output) -> String {
    format!("stdout: {}\nstderr: {}", out(output), err(output))
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_accepts_a_kdl_file_and_summarizes_it() {
    let daemon = Daemon::start().await;
    let file = daemon.file("candidate.kdl", &daemon.text);

    let output = daemon.run(&["config", "validate", path(&file)]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    let text = out(&output);
    assert!(text.contains("valid"), "{text}");
    assert!(text.contains("kdl"), "{text}");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_accepts_every_native_format() {
    let daemon = Daemon::start().await;
    let draft = draft(daemon.address);
    for (name, format, flag) in [
        ("candidate.lua", ConfigFormat::Lua, None),
        ("candidate.hocon", ConfigFormat::Hocon, None),
        ("candidate.uci", ConfigFormat::Uci, None),
        ("candidate.json", ConfigFormat::Hocon, Some("json")),
        ("candidate.txt", ConfigFormat::Kdl, Some("kdl")),
    ] {
        let file = daemon.file(name, &rendered(draft.clone(), format));
        let mut words = vec!["config", "validate", path(&file)];
        words.extend(flag.into_iter().flat_map(|flag| ["--format", flag]));

        let output = daemon.run(&words);

        assert_eq!(code(&output), 0, "{name}: {}", report(&output));
    }
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_file_exits_3_with_code_line_and_hint() {
    let daemon = Daemon::start().await;
    let file = daemon.file("broken.kdl", &format!("{}\nlisteners {{\n", daemon.text));

    let output = daemon.run(&["config", "validate", path(&file)]);

    assert_eq!(code(&output), 3, "{}", report(&output));
    let text = err(&output);
    assert!(text.contains("is invalid"), "{text}");
    assert!(text.contains("E_SYNTAX"), "{text}");
    assert!(text.contains("broken.kdl:"), "{text}");
    assert!(text.contains("hint:"), "{text}");
    assert!(!text.contains("startup failed"), "{text}");
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_file_is_a_local_input_error() {
    let daemon = Daemon::start().await;
    let output = daemon.run(&["config", "validate", "/nonexistent/oxiroute.kdl"]);
    assert_eq!(code(&output), 3, "{}", report(&output));
    assert!(
        err(&output).contains("cannot read configuration"),
        "{}",
        err(&output)
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_lists_changes_by_name_and_exits_1_only_when_asked() {
    let daemon = Daemon::start().await;
    let same = daemon.file("same.kdl", &daemon.text);
    let edited = daemon
        .text
        .replacen("max_connections 4", "max_connections 9", 1);
    let changed = daemon.file("changed.kdl", &edited);

    let equal = daemon.run(&["config", "diff", "--exit-code", path(&same)]);
    let different = daemon.run(&["config", "diff", "--exit-code", path(&changed)]);
    let ignoring = daemon.run(&["config", "diff", path(&changed)]);

    assert_eq!(code(&equal), 0, "{}", report(&equal));
    assert_eq!(out(&equal), "");
    assert!(err(&equal).contains("no differences"), "{}", err(&equal));
    assert_eq!(code(&different), 1, "{}", report(&different));
    assert_eq!(
        out(&different),
        "~ /upstream_pools/farm/servers/alpha/max_connections: 4 -> 9\n"
    );
    assert_eq!(err(&different), "");
    assert_eq!(code(&ignoring), 0, "{}", report(&ignoring));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_text_prints_the_unified_diff_and_counts_comment_changes() {
    let daemon = Daemon::start().await;
    let file = daemon.file("noted.kdl", &format!("{}// one more note\n", daemon.text));

    let plain = daemon.run(&["config", "diff", "--exit-code", path(&file)]);
    let text = daemon.run(&["config", "diff", "--exit-code", "--text", path(&file)]);

    assert_eq!(code(&plain), 0, "{}", report(&plain));
    assert_eq!(code(&text), 1, "{}", report(&text));
    assert!(
        out(&text).starts_with("--- disk\n+++ candidate\n@@ "),
        "{}",
        out(&text)
    );
    assert!(out(&text).contains("+// one more note\n"), "{}", out(&text));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_against_disk_and_json_output() {
    let daemon = Daemon::start().await;
    let edited = daemon
        .text
        .replacen("max_connections 4", "max_connections 9", 1);
    let file = daemon.file("changed.kdl", &edited);

    let output = daemon.run(&[
        "--output",
        "json",
        "config",
        "diff",
        "--against",
        "disk",
        path(&file),
    ]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    let body: Value = serde_json::from_slice(&output.stdout).expect("JSON body");
    assert_eq!(body["diff"]["disk"]["changes"][0]["new"], 9);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_dry_run_writes_nothing() {
    let daemon = Daemon::start().await;
    let edited = daemon
        .text
        .replacen("max_connections 4", "max_connections 9", 1);
    let file = daemon.file("changed.kdl", &edited);

    let output = daemon.run(&["config", "apply", "--dry-run", path(&file)]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert!(
        err(&output).contains("nothing was written"),
        "{}",
        err(&output)
    );
    assert_eq!(daemon.stored(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_wait_stores_the_text_and_returns_once_it_is_active() {
    let daemon = Daemon::start().await;
    let authorization = authorization();
    let original = active_revision(daemon.address, &authorization).await;
    let edited = format!(
        "{}// applied\n",
        daemon
            .text
            .replacen("max_connections 4", "max_connections 9", 1)
    );
    let file = daemon.file("changed.kdl", &edited);

    let output = daemon.run(&["config", "apply", "--wait", path(&file)]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert_eq!(daemon.stored(), edited);
    assert!(
        out(&output).contains("saved_pending_activation"),
        "{}",
        out(&output)
    );
    let after = active_revision(daemon.address, &authorization).await;
    assert_ne!(
        after, original,
        "the new revision is active when --wait returns"
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn applying_the_stored_text_again_is_a_no_op() {
    let daemon = Daemon::start().await;
    let file = daemon.file("same.kdl", &daemon.text);

    let output = daemon.run(&["config", "apply", "--wait", path(&file)]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert!(
        err(&output).contains("identical to the stored configuration: nothing was written"),
        "{}",
        err(&output)
    );
    assert_eq!(daemon.stored(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_another_format_unless_converted() {
    let daemon = Daemon::start().await;
    let authorization = authorization();
    let original = active_revision(daemon.address, &authorization).await;
    let mut other = draft(daemon.address);
    other.upstream_pools[0].servers[0].max_connections = Some(9);
    let file = daemon.file("candidate.lua", &rendered(other, ConfigFormat::Lua));

    let refused = daemon.run(&["config", "apply", path(&file)]);
    let converted = daemon.run(&["config", "apply", "--convert", "--wait", path(&file)]);

    assert_eq!(code(&refused), 3, "{}", report(&refused));
    assert!(err(&refused).contains("--convert"), "{}", err(&refused));
    assert_eq!(code(&converted), 0, "{}", report(&converted));
    assert!(
        daemon.stored().contains("max_connections 9"),
        "{}",
        daemon.stored()
    );
    assert_ne!(
        active_revision(daemon.address, &authorization).await,
        original
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_typed_json_document_still_applies_to_a_kdl_daemon() {
    let daemon = Daemon::start().await;
    let authorization = authorization();
    let original = active_revision(daemon.address, &authorization).await;
    let got = daemon.run(&["--output", "json", "config", "get"]);
    let mut document: Value = serde_json::from_slice(&got.stdout).expect("config get JSON");
    document["config"]["upstream_pools"][0]["servers"][0]["max_connections"] = json!(9);
    let file = daemon.file("typed.json", &document.to_string());

    let output = daemon.run(&["config", "apply", "--wait", path(&file)]);

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert!(daemon.stored().contains("max_connections 9"));
    assert!(
        !daemon.stored().contains("must survive"),
        "a typed document replaces the comments"
    );
    assert_ne!(
        active_revision(daemon.address, &authorization).await,
        original
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_names_the_fix_when_the_daemon_cannot_write() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let daemon = Daemon::start().await;
    let directory = daemon
        .server
        .config_path
        .parent()
        .expect("directory")
        .to_owned();
    let file = daemon.file("changed.kdl", &format!("{}// x\n", daemon.text));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).expect("lock directory");

    let output = daemon.run(&["config", "apply", path(&file)]);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("unlock directory");

    assert_eq!(code(&output), 8, "{}", report(&output));
    let text = err(&output);
    assert!(
        text.contains("cannot replace its configuration file"),
        "{text}"
    );
    assert!(text.contains("write access"), "{text}");
    assert_eq!(daemon.stored(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn show_prints_the_authored_text_the_active_rendering_and_a_local_file() {
    let daemon = Daemon::start().await;

    let stored = daemon.run(&["config", "show"]);
    let active = daemon.run(&["config", "show", "--active"]);
    let local = daemon.run(&["config", "show", path(&daemon.server.config_path)]);
    let json_output = daemon.run(&["--output", "json", "config", "show"]);

    assert_eq!(out(&stored), daemon.text);
    assert!(!out(&active).contains("must survive"), "{}", out(&active));
    assert!(
        err(&active).contains("rendered canonically"),
        "{}",
        err(&active)
    );
    assert_eq!(out(&local), daemon.text);
    let body: Value = serde_json::from_slice(&json_output.stdout).expect("JSON body");
    assert_eq!(body["text"], daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_applies_the_edited_text_with_its_comments() {
    let daemon = Daemon::start().await;
    let editor = daemon.editor(
        "sed.sh",
        "sed -i 's/max_connections 4/max_connections 9/; $a // edited' \"$1\"",
    );

    let output = daemon
        .command(&["config", "edit", "--yes", "--wait"])
        .env("EDITOR", &editor)
        .output()
        .expect("config edit");

    assert_eq!(code(&output), 0, "{}", report(&output));
    let stored = daemon.stored();
    assert!(stored.contains("must survive"));
    assert!(stored.contains("max_connections 9"));
    assert!(stored.ends_with("// edited\n"), "{stored}");
    assert!(
        out(&output).contains("/upstream_pools/farm/servers/alpha/max_connections"),
        "{}",
        out(&output)
    );
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_prefers_visual_and_an_empty_edit_cancels() {
    let daemon = Daemon::start().await;
    let visual = daemon.editor("empty.sh", ": > \"$1\"");
    let ignored = daemon.work.path().join("not-an-editor");

    let output = daemon
        .command(&["config", "edit", "--yes"])
        .env("VISUAL", &visual)
        .env("EDITOR", &ignored)
        .output()
        .expect("config edit");

    assert_eq!(code(&output), 0, "{}", report(&output));
    assert!(err(&output).contains("empty"), "{}", err(&output));
    assert_eq!(daemon.stored(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_without_a_terminal_needs_yes_and_keeps_the_edit_when_it_is_invalid() {
    let daemon = Daemon::start().await;
    let unconfirmed = daemon.editor("unconfirmed.sh", "echo '// note' >> \"$1\"");
    let broken = daemon.editor("broken.sh", "echo 'listeners {' >> \"$1\"");

    let asked = daemon
        .command(&["config", "edit"])
        .env("EDITOR", &unconfirmed)
        .output()
        .expect("config edit");
    let refused = daemon
        .command(&["config", "edit", "--yes"])
        .env("EDITOR", &broken)
        .output()
        .expect("config edit");

    assert_eq!(code(&asked), 3, "{}", report(&asked));
    assert!(err(&asked).contains("pass --yes"), "{}", err(&asked));
    assert_eq!(code(&refused), 3, "{}", report(&refused));
    assert!(err(&refused).contains("E_SYNTAX"), "{}", err(&refused));
    let kept = err(&refused)
        .split("your edit is kept in ")
        .nth(1)
        .map(|rest| rest.trim().trim_end_matches(')').to_owned())
        .expect("the kept path");
    assert!(
        fs::read_to_string(&kept)
            .expect("kept edit")
            .ends_with("listeners {\n")
    );
    fs::remove_file(kept).expect("remove kept edit");
    assert_eq!(daemon.stored(), daemon.text);
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_over_a_concurrent_change_exits_7_and_keeps_the_edit() {
    let daemon = Daemon::start().await;
    let stored = &daemon.server.config_path;
    let editor = daemon.editor(
        "racing.sh",
        &format!(
            "echo '// note' >> \"$1\"\necho '// changed meanwhile' >> '{}'",
            stored.display()
        ),
    );

    let output = daemon
        .command(&["config", "edit", "--yes"])
        .env("EDITOR", &editor)
        .output()
        .expect("config edit");

    assert_eq!(code(&output), 7, "{}", report(&output));
    assert!(
        err(&output).contains("changed since it was read"),
        "{}",
        err(&output)
    );
    assert!(daemon.stored().ends_with("// changed meanwhile\n"));
    let kept = err(&output)
        .split("your edit is kept in ")
        .nth(1)
        .map(|rest| rest.trim().trim_end_matches(')').to_owned())
        .expect("the kept path");
    fs::remove_file(kept).expect("remove kept edit");
    daemon.server.shutdown();
}

#[test]
fn fmt_rewrites_a_kdl_file_to_its_canonical_form_and_drops_comments() {
    let directory = TempDir::new().expect("directory");
    let address = reserve_tcp_address();
    let canonical = rendered(draft(address), ConfigFormat::Kdl);
    let noisy = format!("// a comment that fmt does not keep\n{canonical}");
    let file = directory.path().join("oxiroute.kdl");
    fs::write(&file, &noisy).expect("write file");
    let cli = |words: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .args(words)
            .output()
            .expect("oxiroute CLI")
    };

    let printed = cli(&["config", "fmt", path(&file)]);
    let check = cli(&["config", "fmt", "--check", path(&file)]);
    let diff = cli(&["config", "fmt", "--diff", path(&file)]);
    let written = cli(&["config", "fmt", "--write", path(&file)]);
    let again = cli(&["config", "fmt", "--check", path(&file)]);

    assert_eq!(code(&printed), 0, "{}", report(&printed));
    assert_eq!(out(&printed), canonical);
    assert_eq!(code(&check), 1, "{}", report(&check));
    assert_eq!(out(&check), "");
    assert_eq!(code(&diff), 1, "{}", report(&diff));
    assert!(
        out(&diff).contains("-// a comment that fmt does not keep\n"),
        "{}",
        out(&diff)
    );
    assert_eq!(code(&written), 0, "{}", report(&written));
    assert_eq!(
        fs::read_to_string(&file).expect("formatted file"),
        canonical
    );
    assert_eq!(code(&again), 0, "{}", report(&again));
}

#[test]
fn fmt_refuses_templates_and_json_and_reports_syntax_errors() {
    let directory = TempDir::new().expect("directory");
    let templated = directory.path().join("templated.hocon");
    fs::write(
        &templated,
        "templates = { empty = { listeners = [] } }\nuse = \"empty\"\nversion = 1\n",
    )
    .expect("write file");
    let json_file = directory.path().join("document.json");
    fs::write(&json_file, "{}").expect("write file");
    let broken = directory.path().join("broken.kdl");
    fs::write(&broken, "listeners {\n").expect("write file");
    let cli = |file: &Path| {
        Command::new(env!("CARGO_BIN_EXE_oxiroute"))
            .args(["config", "fmt", path(file)])
            .output()
            .expect("oxiroute CLI")
    };

    for (file, needle) in [
        (&templated, "templates or native references"),
        (&json_file, "JSON document"),
        (&broken, "broken.kdl"),
    ] {
        let output = cli(file);
        assert_eq!(code(&output), 3, "{}", report(&output));
        assert!(err(&output).contains(needle), "{}", err(&output));
    }
}

fn history_ids(daemon: &Daemon) -> Vec<String> {
    let output = daemon.run(&["--output", "json", "config", "history"]);
    assert_eq!(code(&output), 0, "{}", report(&output));
    let body: Value = serde_json::from_slice(&output.stdout).expect("history JSON");
    body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn history_lists_the_versions_an_apply_replaced_and_show_prints_one() {
    let daemon = Daemon::start().await;
    let first = format!("{}// first\n", daemon.text);
    let second = format!("{first}// second\n");
    for text in [&first, &second] {
        let file = daemon.file("next.kdl", text);
        let output = daemon.run(&["config", "apply", path(&file)]);
        assert_eq!(code(&output), 0, "{}", report(&output));
    }

    let ids = history_ids(&daemon);
    assert_eq!(ids.len(), 2, "{ids:?}");
    let table = daemon.run(&["config", "history"]);
    assert_eq!(code(&table), 0, "{}", report(&table));
    assert!(
        out(&table).contains(&ids[0]) && out(&table).contains(&ids[1]),
        "{}",
        out(&table)
    );
    let original = daemon.run(&["config", "show", "--rev", &ids[1]]);
    assert_eq!(out(&original), daemon.text, "{}", report(&original));
    let prefix = daemon.run(&["config", "show", "--rev", &ids[0][..7]]);
    assert_eq!(out(&prefix), first, "{}", report(&prefix));
    let current = daemon.run(&["config", "show", "--rev", "current"]);
    assert_eq!(out(&current), second);
    let unknown = daemon.run(&["config", "show", "--rev", "000000000000"]);
    assert_eq!(code(&unknown), 6, "{}", report(&unknown));
    let short = daemon.run(&["config", "show", "--rev", "abc"]);
    assert_eq!(code(&short), 8, "{}", report(&short));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_rev_compares_two_versions_by_setting_and_by_text() {
    let daemon = Daemon::start().await;
    let edited = daemon
        .text
        .replacen("max_connections 4", "max_connections 9", 1);
    let file = daemon.file("changed.kdl", &edited);
    assert_eq!(code(&daemon.run(&["config", "apply", path(&file)])), 0);
    let original = history_ids(&daemon).remove(0);

    let settings = daemon.run(&["config", "diff", "--exit-code", "--rev", &original]);
    let text = daemon.run(&[
        "config", "diff", "--text", "--rev", &original, "--rev", "current",
    ]);
    let same = daemon.run(&[
        "config",
        "diff",
        "--exit-code",
        "--rev",
        "current",
        "--rev",
        "current",
    ]);
    let conflicting = daemon.run(&["config", "diff", "--rev", &original, path(&file)]);

    assert_eq!(code(&settings), 1, "{}", report(&settings));
    assert_eq!(
        out(&settings),
        "~ /upstream_pools/farm/servers/alpha/max_connections: 4 -> 9\n"
    );
    assert_eq!(code(&text), 0, "{}", report(&text));
    assert!(out(&text).contains("+++ current"), "{}", out(&text));
    assert!(
        out(&text).contains("-        max_connections 4"),
        "{}",
        out(&text)
    );
    assert_eq!(code(&same), 0, "{}", report(&same));
    assert_eq!(code(&conflicting), 2, "{}", report(&conflicting));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_restores_a_version_as_a_new_revision_and_waits_for_it() {
    let daemon = Daemon::start().await;
    let authorization = authorization();
    let original = active_revision(daemon.address, &authorization).await;
    let bad = format!(
        "{}// the bad one\n",
        daemon
            .text
            .replacen("max_connections 4", "max_connections 1", 1)
    );
    let file = daemon.file("bad.kdl", &bad);
    let applied = daemon.run(&["config", "apply", "--wait", path(&file)]);
    assert_eq!(code(&applied), 0, "{}", report(&applied));
    let before = history_ids(&daemon).remove(0);

    let restored = daemon.run(&["config", "rollback", &before, "--wait"]);

    assert_eq!(code(&restored), 0, "{}", report(&restored));
    assert_eq!(daemon.stored(), daemon.text);
    assert_eq!(
        active_revision(daemon.address, &authorization).await,
        original
    );
    let kept = daemon.run(&["config", "show", "--rev", &history_ids(&daemon)[0]]);
    assert_eq!(
        out(&kept),
        bad,
        "the replaced bad version is the newest entry"
    );
    let unknown = daemon.run(&["config", "rollback", "000000000000"]);
    assert_eq!(code(&unknown), 6, "{}", report(&unknown));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rollback_to_a_version_that_no_longer_validates_changes_nothing() {
    let daemon = Daemon::start().await;
    let file = daemon.file("next.kdl", &format!("{}// next\n", daemon.text));
    assert_eq!(code(&daemon.run(&["config", "apply", path(&file)])), 0);
    let id = history_ids(&daemon).remove(0);
    let mut history = daemon.server.config_path.clone().into_os_string();
    history.push(".history");
    let entry = fs::read_dir(PathBuf::from(history))
        .expect("history directory")
        .next()
        .expect("entry")
        .expect("entry")
        .path();
    fs::write(entry, "listeners {\n").expect("corrupt the entry");
    let stored = daemon.stored();

    let refused = daemon.run(&["config", "rollback", &id]);

    assert_eq!(code(&refused), 3, "{}", report(&refused));
    assert!(err(&refused).contains("E_SYNTAX"), "{}", err(&refused));
    assert_eq!(daemon.stored(), stored);
    daemon.server.shutdown();
}

fn overlay_file(daemon: &Daemon) -> PathBuf {
    let mut path = daemon.server.config_path.clone().into_os_string();
    path.push(".overlay.json");
    PathBuf::from(path)
}

#[tokio::test(flavor = "multi_thread")]
async fn persist_keeps_a_change_in_the_overlay_and_every_report_names_it() {
    let daemon = Daemon::start().await;

    let drained = daemon.run(&[
        "--output",
        "plain",
        "server",
        "drain",
        "--pool",
        "farm",
        "alpha",
        "--persist",
    ]);

    assert_eq!(code(&drained), 0, "{}", report(&drained));
    assert_eq!(
        out(&drained),
        "target=farm/alpha setting=state value=drain change=changed persistence=overlay\n"
    );
    assert_eq!(daemon.stored(), daemon.text);
    let shown = daemon.run(&["--output", "plain", "overlay", "show"]);
    assert_eq!(code(&shown), 0, "{}", report(&shown));
    let text = out(&shown);
    assert!(text.contains("state=applied"), "{text}");
    assert!(
        text.contains("target=farm/alpha setting=state value=drain status=applied"),
        "{text}"
    );
    let status = daemon.run(&["status"]);
    assert!(
        err(&status).contains("persistence overlay: 1 setting(s)"),
        "{}",
        report(&status)
    );
    let file = daemon.file("same.kdl", &daemon.text);
    let diff = daemon.run(&["config", "diff", "--exit-code", path(&file)]);
    assert_eq!(code(&diff), 0, "{}", report(&diff));
    assert!(
        err(&diff).contains("persistence overlay"),
        "{}",
        report(&diff)
    );
    let plain = daemon.run(&[
        "--output", "plain", "server", "ready", "--pool", "farm", "alpha",
    ]);
    assert_eq!(
        out(&plain),
        "target=farm/alpha setting=state value=ready change=changed\n"
    );
    let diverged = daemon.run(&["--output", "plain", "overlay", "show"]);
    assert!(
        out(&diverged).contains("status=diverged"),
        "{}",
        report(&diverged)
    );

    let cleared = daemon.run(&["overlay", "clear"]);

    assert_eq!(code(&cleared), 0, "{}", report(&cleared));
    assert!(!overlay_file(&daemon).exists());
    let empty = daemon.run(&["--output", "plain", "overlay", "show"]);
    assert!(out(&empty).contains("state=empty"), "{}", report(&empty));
    daemon.server.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn promote_writes_the_persisted_limits_into_the_file_and_empties_the_overlay() {
    let daemon = Daemon::start().await;
    let limit = daemon.run(&[
        "server",
        "max-connections",
        "set",
        "--pool",
        "farm",
        "alpha",
        "9",
        "--persist",
    ]);
    assert_eq!(code(&limit), 0, "{}", report(&limit));
    let weight = daemon.run(&[
        "server",
        "weight",
        "set",
        "--pool",
        "farm",
        "alpha",
        "3",
        "--persist",
    ]);
    assert_eq!(code(&weight), 0, "{}", report(&weight));

    let promoted = daemon.run(&["overlay", "promote", "--wait"]);

    assert_eq!(code(&promoted), 0, "{}", report(&promoted));
    let stored = daemon.stored();
    assert!(stored.contains("max_connections 9"), "{stored}");
    assert!(!stored.contains("max_connections 3"), "{stored}");
    let shown = daemon.run(&["--output", "plain", "overlay", "show"]);
    let text = out(&shown);
    assert!(
        text.contains("setting=weight value=3 status=applied"),
        "{text}"
    );
    assert!(!text.contains("max_connections"), "{text}");
    let nothing = daemon.run(&["overlay", "promote"]);
    assert_eq!(code(&nothing), 0, "{}", report(&nothing));
    assert!(
        out(&nothing).contains("nothing_to_promote"),
        "{}",
        report(&nothing)
    );
    daemon.server.shutdown();
}
