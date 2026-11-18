//! `oxiroute doctor` against a real daemon and configuration files with real problems.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Output,
};

use oxiroute_config::CacheStore;
use serde_json::Value;
use tempfile::TempDir;

use super::{
    Daemon, backend, cli_command, config, config_support::render_lua, reserve_tcp_address, stderr,
    stdout,
};

const OPEN_UNIT: &str = "User=oxiroute\nAmbientCapabilities=CAP_NET_BIND_SERVICE\n";
const SANDBOXED_UNIT: &str = "ProtectSystem=strict\nReadWritePaths=-/nonexistent\n";

fn unit(directory: &Path, daemon: &Daemon, settings: &str) -> PathBuf {
    let token = daemon.token_file.display();
    let text = format!("[Service]\n{settings}Environment=OXIROUTE_MANAGEMENT_TOKEN_FILE={token}\n");
    write(directory, "oxiroute.service", &text, 0o644)
}

fn write(directory: &Path, name: &str, text: &str, mode: u32) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, text).expect("file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    path
}

fn contexts_home(unit: &Path) -> PathBuf {
    unit.parent().expect("unit directory").join("home")
}

fn doctor(daemon: &Daemon, config: &Path, unit: &Path, extra: &[&str]) -> Output {
    cli_command()
        .env_remove("OXIROUTE_CONTEXT")
        .env("XDG_CONFIG_HOME", contexts_home(unit))
        .args(daemon.connection())
        .args(["doctor"])
        .arg(config)
        .arg("--unit")
        .arg(unit)
        .args(extra)
        .output()
        .expect("oxiroute CLI")
}

fn findings(output: &Output) -> Vec<Value> {
    let document: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    document["findings"].as_array().expect("findings").clone()
}

#[tokio::test]
async fn a_healthy_deployment_passes_and_its_own_addresses_are_not_a_problem() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let unit = unit(directory.path(), &daemon, OPEN_UNIT);

    let healthy = doctor(&daemon, &daemon.server.config_path, &unit, &["--json"]);
    assert_eq!(healthy.status.code(), Some(0), "{}", stdout(&healthy));
    let checks = findings(&healthy)
        .iter()
        .map(|finding| finding["check"].as_str().expect("check").to_owned())
        .collect::<Vec<_>>();
    for check in [
        "daemon",
        "config",
        "files",
        "listeners",
        "upstreams",
        "unit",
    ] {
        assert!(
            checks.iter().any(|name| name == check),
            "{check}: {checks:?}"
        );
    }

    let offline = doctor(&daemon, &daemon.server.config_path, &unit, &["--offline"]);
    assert_eq!(offline.status.code(), Some(2), "{}", stdout(&offline));
    let text = stdout(&offline);
    assert!(text.starts_with("OXIROUTE DOCTOR CRITICAL"), "{text}");
    assert!(
        text.contains("already in use") && text.contains("ss -ltnp"),
        "{text}"
    );
}

#[tokio::test]
async fn problems_are_listed_worst_first_each_with_its_fix() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let cache = directory.path().join("cache");
    fs::create_dir(&cache).expect("cache directory");
    let alive = backend().await;
    let dead = {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        closed.local_addr().expect("address")
    };
    let mut draft = config(reserve_tcp_address(), reserve_tcp_address(), [alive, dead]);
    draft.cache_stores = vec![CacheStore::disk("pages", cache.clone(), 1 << 20)];
    let validated = draft.validate().expect("valid configuration");
    let file = write(
        directory.path(),
        "oxiroute.lua",
        &render_lua(&validated).expect("Lua"),
        0o666,
    );
    let unit = unit(directory.path(), &daemon, SANDBOXED_UNIT);

    let output = doctor(&daemon, &file, &unit, &["--json", "--offline"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let found = findings(&output);
    let severities = found
        .iter()
        .map(|finding| finding["severity"].as_str().expect("severity"))
        .collect::<Vec<_>>();
    let mut sorted = severities.clone();
    sorted.sort_by_key(|severity| match *severity {
        "critical" => 0,
        "warning" => 1,
        _ => 2,
    });
    assert_eq!(severities, sorted, "{found:?}");
    let message = |check: &str, severity: &str| {
        found
            .iter()
            .find(|finding| finding["check"] == check && finding["severity"] == severity)
    };
    let mode = message("files", "critical").expect("a configuration open to every user");
    assert!(
        mode["fix"].as_str().expect("fix").starts_with("chmod o-w"),
        "{mode}"
    );
    let pool = message("upstreams", "warning").expect("a pool with one dead server");
    assert!(
        pool["message"].as_str().expect("message").contains("beta"),
        "{pool}"
    );
    let sandbox = message("unit", "critical").expect("a cache the unit cannot write");
    assert!(
        sandbox["fix"]
            .as_str()
            .expect("fix")
            .contains("ReadWritePaths="),
        "{sandbox}"
    );

    let text = doctor(&daemon, &file, &unit, &["--offline"]);
    let text = stdout(&text);
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("CRITICAL")),
        "{text}"
    );
    assert!(text.contains("\n    fix: "), "{text}");
}

fn contexts_file(unit: &Path, text: &str, mode: u32) {
    let directory = contexts_home(unit).join("oxiroute");
    fs::create_dir_all(&directory).expect("contexts directory");
    write(&directory, "contexts", text, mode);
}

fn contexts_findings(output: &Output) -> Vec<Value> {
    findings(output)
        .into_iter()
        .filter(|finding| finding["check"] == "contexts")
        .collect()
}

#[tokio::test]
async fn the_contexts_file_and_the_token_files_it_names_are_checked() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let unit = unit(directory.path(), &daemon, OPEN_UNIT);
    let loose = write(directory.path(), "loose.token", &"a".repeat(64), 0o644);
    let good = daemon.token_file.display();
    let text = format!(
        r#"{{"schemaVersion": 1, "current": "prod", "contexts": {{
            "prod": {{"endpoint": "http://192.0.2.10:9900", "token": {{"file": "{loose}"}}}},
            "staging": {{"endpoint": "http://192.0.2.11:9900", "token": {{"file": "{good}"}}}},
            "lab": {{"endpoint": "http://192.0.2.12:9900", "token": {{"file": "{loose}"}}}}
        }}}}"#,
        loose = loose.display()
    );

    contexts_file(&unit, &text, 0o600);
    let run = || doctor(&daemon, &daemon.server.config_path, &unit, &["--json"]);
    let found = contexts_findings(&run());
    let severity_of = |subject: &str| {
        found
            .iter()
            .find(|finding| finding["subject"] == subject)
            .map(|finding| finding["severity"].as_str().expect("severity"))
    };
    assert_eq!(severity_of("context prod"), Some("critical"), "{found:?}");
    assert_eq!(severity_of("context lab"), Some("warning"), "{found:?}");
    assert_eq!(severity_of("context staging"), None, "{found:?}");
    assert!(
        found.iter().any(|finding| finding["severity"] == "ok"
            && finding["message"]
                .as_str()
                .is_some_and(|message| message.contains("3 contexts")
                    && message.contains("prod is selected"))),
        "{found:?}"
    );

    contexts_file(&unit, &text, 0o644);
    let open = contexts_findings(&run());
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["severity"], "critical");
    assert!(
        open[0]["message"]
            .as_str()
            .expect("message")
            .contains("mode 0644"),
        "{open:?}"
    );
}

#[tokio::test]
async fn an_unknown_requested_context_is_reported_and_the_rest_of_the_report_still_runs() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let unit = unit(directory.path(), &daemon, OPEN_UNIT);
    let text =
        r#"{"schemaVersion": 1, "contexts": {"prod": {"endpoint": "http://192.0.2.10:9900"}}}"#;
    contexts_file(&unit, text, 0o600);

    let output = doctor(
        &daemon,
        &daemon.server.config_path,
        &unit,
        &["--json", "--context", "nowhere"],
    );
    let found = findings(&output);
    let contexts = contexts_findings(&output);
    assert_eq!(contexts.len(), 1, "{contexts:?}");
    assert!(
        contexts[0]["message"]
            .as_str()
            .expect("message")
            .contains("no context named nowhere"),
        "{contexts:?}"
    );
    assert!(
        found
            .iter()
            .any(|finding| finding["check"] == "upstreams" && finding["severity"] == "warning"),
        "{found:?}"
    );
    assert!(found.iter().any(|finding| finding["check"] == "config"));
}

#[tokio::test]
async fn the_overlay_and_history_beside_the_configuration_are_checked() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let unit = unit(directory.path(), &daemon, OPEN_UNIT);
    let name = daemon
        .server
        .config_path
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();
    let config = directory.path().join(&name);
    fs::copy(&daemon.server.config_path, &config).expect("configuration copy");
    let overlay = write(
        directory.path(),
        &format!("{name}.overlay.json"),
        "{",
        0o644,
    );
    let output = doctor(&daemon, &config, &unit, &["--json"]);
    let flagged = findings(&output)
        .into_iter()
        .filter(|finding| finding["subject"] == overlay.display().to_string().as_str())
        .collect::<Vec<_>>();
    assert_eq!(flagged.len(), 2, "{flagged:?}");
    assert!(
        flagged
            .iter()
            .all(|finding| finding["severity"] == "warning" && finding["check"] == "files")
    );
}

#[tokio::test]
async fn a_missing_configuration_and_bad_options_are_reported_not_crashed_on() {
    let daemon = Daemon::start().await;
    let directory = TempDir::new().expect("directory");
    let unit = unit(directory.path(), &daemon, OPEN_UNIT);

    let missing = doctor(
        &daemon,
        &directory.path().join("absent.kdl"),
        &unit,
        &["--offline"],
    );
    assert_eq!(missing.status.code(), Some(2), "{}", stdout(&missing));
    assert!(
        stdout(&missing).contains("cannot read configuration"),
        "{}",
        stdout(&missing)
    );

    let bad = cli_command()
        .args(["doctor", "--no-such-option"])
        .output()
        .expect("CLI");
    assert_eq!(bad.status.code(), Some(3), "{}", stderr(&bad));
    assert!(
        stdout(&bad).starts_with("OXIROUTE UNKNOWN"),
        "{}",
        stdout(&bad)
    );
}
