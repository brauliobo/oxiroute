use std::{
    fs,
    net::{TcpListener, UdpSocket},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use oxiroute_config::{ConfigDraft, ListenerBind, UpstreamPool};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    diagnosis::{Check, Diagnosis, report},
    files, listeners,
    listeners::Host,
    sidecars,
    unit::{Access, Environment, Unit},
    upstreams, writes,
};
use crate::{
    config::history::{ConfigHistory, HistoryLimits},
    observability::status::summary::Severity,
};

const PACKAGED: &str = "[Unit]
Description=OxiRoute

[Service]
User=oxiroute
EnvironmentFile=-/nonexistent/oxiroute.env
Environment=\"OXIROUTE_AUDIT_DIR=/var/lib/oxiroute/audit\"
WorkingDirectory=/var/lib/oxiroute
ExecStart=/usr/bin/oxiroute serve /etc/oxiroute/oxiroute.kdl
AmbientCapabilities=CAP_NET_BIND_SERVICE
ProtectSystem=strict
StateDirectory=oxiroute
RuntimeDirectory=oxiroute
ReadWritePaths=/run/oxiroute /var/lib/oxiroute
";

fn unit_of(text: &str, dropins: &[&str]) -> Unit {
    let texts = std::iter::once(text)
        .chain(dropins.iter().copied())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Unit::parse(PathBuf::from("oxiroute.service"), &texts)
}

fn config(overrides: &Value) -> ConfigDraft {
    let mut document = json!({
        "version": 1, "listeners": [], "certificates": [], "tls_profiles": [], "cache_stores": [],
        "upstream_pools": [], "http_services": [], "forward_proxy_services": [], "rtmp_services": [],
        "l4_services": [],
    });
    document
        .as_object_mut()
        .expect("object")
        .extend(overrides.as_object().cloned().unwrap_or_default());
    serde_json::from_value(document).expect("configuration")
}

fn severities(diagnoses: &[Diagnosis]) -> Vec<Severity> {
    diagnoses
        .iter()
        .map(|diagnosis| diagnosis.severity)
        .collect()
}

#[test]
fn the_packaged_unit_allows_its_state_and_run_directories_and_nothing_else() {
    let unit = unit_of(PACKAGED, &[]);
    assert_eq!(
        unit.access(Path::new("/var/lib/oxiroute/audit")),
        Access::Writable
    );
    assert_eq!(
        unit.access(Path::new("/run/oxiroute/socket")),
        Access::Writable
    );
    assert_eq!(
        unit.access(Path::new("/etc/oxiroute/audit")),
        Access::ReadOnly
    );
    assert_eq!(
        unit.access(Path::new("/var/log/oxiroute/access.log")),
        Access::ReadOnly
    );
    assert_eq!(
        unit.serves.as_deref(),
        Some(Path::new("/etc/oxiroute/oxiroute.kdl"))
    );
    assert!(unit.grants_privileged_ports);
    assert_eq!(
        unit.environment().get("OXIROUTE_AUDIT_DIR").as_deref(),
        Some(std::ffi::OsStr::new("/var/lib/oxiroute/audit"))
    );
}

#[test]
fn a_dropin_resets_and_extends_the_paths_and_a_weaker_sandbox_blocks_less() {
    let dropin =
        "[Service]\nReadWritePaths=\nReadWritePaths=-/var/log/oxiroute\nProtectSystem=full\n";
    let unit = unit_of(PACKAGED, &[dropin]);
    assert_eq!(
        unit.access(Path::new("/var/log/oxiroute/access.log")),
        Access::Writable
    );
    assert_eq!(
        unit.access(Path::new("/var/cache/oxiroute")),
        Access::Writable
    );
    assert_eq!(
        unit.access(Path::new("/etc/oxiroute/audit")),
        Access::ReadOnly
    );
    assert_eq!(
        unit.access(Path::new("/var/lib/oxiroute")),
        Access::Writable
    );
    assert_eq!(
        unit_of("[Service]\nProtectHome=yes\n", &[]).access(Path::new("/home/alice/cache")),
        Access::Hidden
    );
    assert_eq!(
        unit_of("[Service]\n", &[]).access(Path::new("/anywhere")),
        Access::Writable
    );
}

#[test]
fn a_unit_without_ambient_capabilities_cannot_bind_low_ports_unless_it_runs_as_root() {
    assert!(!unit_of("[Service]\nUser=oxiroute\n", &[]).grants_privileged_ports);
    assert!(unit_of("[Service]\n", &[]).grants_privileged_ports);
}

#[test]
fn the_paths_a_configuration_writes_are_judged_against_the_unit() {
    let draft = config(&json!({
        "listeners": [{
            "name": "edge", "bind": { "type": "unix", "path": "/run/oxiroute/edge.sock" },
            "protocol": "http", "service": "edge",
        }],
        "cache_stores": [{ "type": "disk", "name": "pages", "root_directory": "/var/cache/oxiroute" }],
        "audit": { "dir": "/etc/oxiroute/audit" },
    }));
    let writes = writes::of(
        &draft,
        &Environment::of_process(),
        Path::new("oxiroute.kdl"),
    );
    let diagnoses = writes::judge(&unit_of(PACKAGED, &[]), &writes);
    let problems = diagnoses
        .iter()
        .filter(|diagnosis| diagnosis.severity == Severity::Critical)
        .map(|diagnosis| diagnosis.message.clone())
        .collect::<Vec<_>>();
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|message| message.contains("disk cache pages /var/cache/oxiroute is read-only"))
    );
    assert!(
        problems
            .iter()
            .any(|message| message.contains("audit log /etc/oxiroute/audit"))
    );
    assert!(
        diagnoses[1]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.starts_with("add ReadWritePaths="))
    );
}

#[test]
fn a_read_write_path_that_does_not_exist_stops_the_unit_unless_it_is_optional() {
    let unit = unit_of(
        "[Service]\nReadWritePaths=/nonexistent/required -/nonexistent/optional\n",
        &[],
    );
    let diagnoses = writes::judge(&unit, &[]);
    assert_eq!(
        diagnoses
            .iter()
            .filter(|d| d.severity == Severity::Critical)
            .count(),
        1
    );
    assert!(diagnoses[0].message.contains("/nonexistent/required"));
}

#[test]
fn a_read_write_path_the_unit_creates_itself_need_not_exist_before_it_starts() {
    let unit = unit_of(
        "[Service]\nStateDirectory=oxiroute\nRuntimeDirectory=oxiroute\nReadWritePaths=/run/oxiroute /var/lib/oxiroute\n",
        &[],
    );
    assert_eq!(unit.missing_required().count(), 0);
}

#[test]
fn the_report_lists_the_worst_first_and_exits_by_severity() {
    let mixed = report(vec![
        Diagnosis::ok(Check::Config, "a", "fine"),
        Diagnosis::warning(Check::Files, "b", "loose").fixed_by("chmod 600 b"),
        Diagnosis::critical(Check::Listeners, "c", "busy"),
    ]);
    assert_eq!(mixed.code, 2);
    let lines = mixed.line.lines().collect::<Vec<_>>();
    assert_eq!(
        lines[0],
        "OXIROUTE DOCTOR CRITICAL - 1 critical, 1 warning, 1 ok"
    );
    assert!(lines[1].starts_with("CRITICAL") && lines[2].starts_with("WARNING"));
    assert_eq!(lines[3], "    fix: chmod 600 b");
    assert!(lines[4].starts_with("OK"));
    assert_eq!(mixed.json["status"], "critical");
    assert_eq!(mixed.json["findings"][1]["fix"], "chmod 600 b");
    assert_eq!(
        report(vec![Diagnosis::warning(Check::Files, "b", "x")]).code,
        1
    );
    assert_eq!(report(vec![Diagnosis::ok(Check::Files, "b", "x")]).code, 0);
}

#[test]
fn an_address_in_use_is_a_problem_unless_the_running_daemon_holds_it() {
    let held = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = held.local_addr().expect("address");
    let udp = UdpSocket::bind("127.0.0.1:0").expect("bind");
    let draft = config(&json!({
        "listeners": [
            { "name": "web", "bind": { "type": "socket", "address": address.to_string() }, "protocol": "http", "service": "web" },
            { "name": "quic", "bind": { "type": "udp", "address": udp.local_addr().expect("address").to_string() }, "protocol": "http3", "service": "web" },
        ],
    }));
    let host = |daemon_running| Host {
        daemon_running,
        grants_privileged_ports: Some(true),
    };

    let stranger = listeners::check(&draft, &host(false));
    assert_eq!(
        severities(&stranger),
        [Severity::Critical, Severity::Critical]
    );
    assert!(stranger[0].message.contains("already in use"));
    assert!(
        stranger[0]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("ss -ltnp"))
    );
    assert!(
        stranger[1]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("ss -lunp"))
    );

    let daemon = listeners::check(&draft, &host(true));
    assert_eq!(severities(&daemon), [Severity::Ok]);
    drop((held, udp));
    assert_eq!(
        severities(&listeners::check(&draft, &host(false))),
        [Severity::Ok]
    );
}

#[test]
fn a_socket_path_in_a_missing_directory_cannot_be_bound() {
    let draft = config(&json!({
        "listeners": [{
            "name": "edge", "bind": { "type": "unix", "path": "/nonexistent/directory/edge.sock" },
            "protocol": "http", "service": "edge",
        }],
    }));
    let host = Host {
        daemon_running: false,
        grants_privileged_ports: None,
    };
    let diagnoses = listeners::check(&draft, &host);
    assert_eq!(severities(&diagnoses), [Severity::Critical]);
    assert!(
        diagnoses[0]
            .message
            .contains("directory of the socket does not exist")
    );
    let bind: ListenerBind =
        serde_json::from_value(json!({ "type": "unix", "path": "/x" })).expect("bind");
    assert_eq!(bind.to_string(), "/x");
}

#[test]
fn a_pool_is_critical_when_no_server_answers_and_a_warning_when_some_do() {
    let alive = TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = {
        let closed = TcpListener::bind("127.0.0.1:0").expect("bind");
        closed.local_addr().expect("address")
    };
    let pool = |name: &str, servers: &[(&str, std::net::SocketAddr)]| -> UpstreamPool {
        serde_json::from_value(json!({
            "name": name,
            "servers": servers.iter().map(|(server, address)| json!({
                "name": server, "endpoint": { "type": "socket", "address": address.to_string() },
            })).collect::<Vec<_>>(),
        }))
        .expect("pool")
    };
    let mut draft = config(&json!({}));
    draft.upstream_pools = vec![
        pool("healthy", &[("a", alive.local_addr().expect("address"))]),
        pool(
            "half",
            &[("a", alive.local_addr().expect("address")), ("b", dead)],
        ),
        pool("down", &[("a", dead)]),
        pool("registered", &[]),
    ];
    let diagnoses = upstreams::check(&draft, Duration::from_secs(2));
    let by_pool = |name: &str| {
        diagnoses
            .iter()
            .filter(|diagnosis| diagnosis.subject == format!("pool {name}"))
            .map(|diagnosis| diagnosis.severity)
            .collect::<Vec<_>>()
    };
    assert_eq!(by_pool("healthy"), [Severity::Ok]);
    assert_eq!(by_pool("half"), [Severity::Warning]);
    assert_eq!(by_pool("down"), [Severity::Critical]);
    assert!(by_pool("registered").is_empty());
}

fn mode(directory: &TempDir, name: &str, mode: u32) -> PathBuf {
    let path = directory.path().join(name);
    fs::write(&path, "secret").expect("file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    path
}

#[test]
fn a_configuration_writable_by_others_and_a_key_readable_by_them_are_reported() {
    let directory = TempDir::new().expect("directory");
    assert_eq!(
        files::config_file(&mode(&directory, "a.kdl", 0o640)).severity,
        Severity::Ok
    );
    assert_eq!(
        files::config_file(&mode(&directory, "b.kdl", 0o664)).severity,
        Severity::Warning
    );
    let open = files::config_file(&mode(&directory, "c.kdl", 0o666));
    assert_eq!(open.severity, Severity::Critical);
    assert!(
        open.fix
            .as_deref()
            .is_some_and(|fix| fix.starts_with("chmod o-w "))
    );
    assert_eq!(
        files::config_file(&directory.path().join("missing.kdl")).severity,
        Severity::Critical
    );

    let private = mode(&directory, "private.key", 0o640);
    let token = mode(&directory, "management.token", 0o644);
    let draft = config(&json!({
        "management": { "bind": "127.0.0.1:9900", "token_file": token },
        "certificates": [{
            "name": "edge", "dns_names": ["example.test"],
            "source": { "type": "files", "certificate_chain_path": "/x.pem", "private_key_path": private },
        }],
    }));
    let diagnoses = files::of_config(&draft, &Environment::of_process());
    assert_eq!(
        severities(&diagnoses),
        [Severity::Critical, Severity::Warning]
    );
    assert!(
        diagnoses[0]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.starts_with("chmod 600 "))
    );
    assert!(
        diagnoses[1]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.starts_with("chmod go-rwx "))
    );
}

const OVERLAY: &str =
    r#"{"schemaVersion": 1, "servers": [{"pool": "web", "server": "a", "weight": 3}]}"#;

fn overlay_beside(directory: &TempDir, text: &str, file_mode: u32) -> PathBuf {
    let path = directory.path().join("oxiroute.kdl.overlay.json");
    fs::write(&path, text).expect("overlay");
    fs::set_permissions(&path, fs::Permissions::from_mode(file_mode)).expect("mode");
    path
}

#[test]
fn a_configuration_without_an_overlay_or_history_reports_nothing_about_them() {
    let directory = TempDir::new().expect("directory");
    assert!(sidecars::check(&directory.path().join("oxiroute.kdl")).is_empty());
}

#[test]
fn a_private_overlay_is_reported_with_its_settings() {
    let directory = TempDir::new().expect("directory");
    overlay_beside(&directory, OVERLAY, 0o600);
    let diagnoses = sidecars::check(&directory.path().join("oxiroute.kdl"));
    assert_eq!(severities(&diagnoses), [Severity::Ok]);
    assert!(diagnoses[0].message.contains("1 persisted settings"));
}

#[test]
fn an_overlay_open_to_others_or_unreadable_as_an_overlay_is_a_warning_with_a_fix() {
    let directory = TempDir::new().expect("directory");
    let path = overlay_beside(&directory, "{", 0o644);
    let diagnoses = sidecars::check(&directory.path().join("oxiroute.kdl"));
    assert_eq!(
        severities(&diagnoses),
        [Severity::Warning, Severity::Warning]
    );
    let fixes = diagnoses
        .iter()
        .map(|diagnosis| diagnosis.fix.clone().expect("fix"))
        .collect::<Vec<_>>();
    assert_eq!(fixes[0], format!("chmod 600 {}", path.display()));
    assert!(fixes[1].contains("oxiroute overlay clear"), "{fixes:?}");
    assert!(diagnoses[1].message.contains("refuses --persist"));
}

#[test]
fn a_history_directory_is_private_only_when_it_and_its_versions_are() {
    let directory = TempDir::new().expect("directory");
    let config = directory.path().join("oxiroute.kdl");
    let history = ConfigHistory::beside(&config);
    history
        .record(b"listeners {}\n", 1_000, HistoryLimits::default())
        .expect("record");
    assert_eq!(severities(&sidecars::check(&config)), [Severity::Ok]);

    let version = fs::read_dir(history.directory())
        .expect("listing")
        .next()
        .expect("version")
        .expect("entry")
        .path();
    fs::set_permissions(&version, fs::Permissions::from_mode(0o644)).expect("mode");
    let diagnoses = sidecars::check(&config);
    assert_eq!(severities(&diagnoses), [Severity::Warning]);
    assert_eq!(diagnoses[0].subject, version.display().to_string());
}

#[test]
fn the_directory_the_management_api_writes_beside_the_configuration_degrades_to_a_warning() {
    let draft = config(&json!({ "management": { "bind": "127.0.0.1:9900" } }));
    let writes = writes::of(
        &draft,
        &Environment::of_process(),
        Path::new("/etc/oxiroute/oxiroute.kdl"),
    );
    let diagnoses = writes::judge(&unit_of(PACKAGED, &[]), &writes);
    assert_eq!(severities(&diagnoses), [Severity::Warning]);
    assert!(diagnoses[0].message.contains("configuration directory"));
    assert_eq!(
        diagnoses[0].fix.as_deref(),
        Some(
            "add ReadWritePaths=/etc/oxiroute to the unit (systemctl edit oxiroute.service) and restart it"
        )
    );

    let granted = unit_of(PACKAGED, &["[Service]\nReadWritePaths=/etc/oxiroute\n"]);
    assert_eq!(
        severities(&writes::judge(&granted, &writes)),
        [Severity::Ok]
    );
}
