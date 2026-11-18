use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use oxiroute_config::{
    ConfigDraft, ListenerBind, Management, Registry, RegistryEndpoint, RegistryRole,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

use crate::http_support::{HttpResponse, http_request};
use crate::process_support::{ServerProcess, reserve_tcp_address, write_config};
use crate::shared::{TOKEN, rtmp_listener_config};

const WAIT: Duration = Duration::from_secs(20);

struct Daemon {
    server: ServerProcess,
    management: SocketAddr,
    socket: PathBuf,
    active: ConfigDraft,
    _directory: TempDir,
}

impl Daemon {
    async fn start(supervised: bool) -> Self {
        Self::launch(supervised, false).await
    }

    async fn launch(supervised: bool, registry: bool) -> Self {
        let directory = TempDir::new().expect("restart case directory");
        let socket = directory.path().join("listener.sock");
        let management = reserve_tcp_address();
        let mut active = rtmp_listener_config(ListenerBind::Unix {
            path: socket.clone(),
            mode: Some(0o600),
        });
        active.management = Some(Management {
            bind: management,
            ui_dir: None,
            token_file: None,
        });
        if registry {
            let state_dir = directory.path().join("state");
            fs::create_dir(&state_dir).expect("registry state directory");
            active.registry = Some(Registry {
                state_dir,
                tokens_file: None,
                endpoints: Vec::new(),
            });
        }
        let validated = active.clone().validate().expect("valid active config");
        let mut server = if supervised {
            ServerProcess::start_supervised(&validated, TOKEN)
        } else {
            ServerProcess::start(&validated, Some(TOKEN))
        };
        server.wait_for_tcp(management).await;
        server.wait_for_unix(&socket).await;
        Self {
            server,
            management,
            socket,
            active,
            _directory: directory,
        }
    }

    fn candidate(&self, mode: u16, max_connections: u64) -> ConfigDraft {
        let mut candidate = self.active.clone();
        candidate.listeners[0].bind = ListenerBind::Unix {
            path: self.socket.clone(),
            mode: Some(mode),
        };
        candidate.listeners[0].max_connections = Some(max_connections);
        candidate
    }

    fn write(&self, config: &ConfigDraft) {
        write_config(
            &self.server.config_path,
            &config.clone().validate().expect("valid candidate"),
        );
    }

    fn hang_up(&self) {
        let status = Command::new("kill")
            .arg("-HUP")
            .arg(self.server.pid().to_string())
            .status()
            .expect("signal the server");
        assert!(status.success());
    }

    async fn request(&self, method: &str, path: &str, body: &Value) -> HttpResponse {
        let authorization = format!("Bearer {TOKEN}");
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body).expect("request body")
        };
        http_request(
            self.management,
            method,
            path,
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
                ("If-Config-Revision", &self.disk_revision().await),
            ],
            &body,
        )
        .await
    }

    async fn disk_revision(&self) -> String {
        let authorization = format!("Bearer {TOKEN}");
        let snapshot = http_request(
            self.management,
            "GET",
            "/api/v1/config",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json();
        snapshot["diskRevision"]
            .as_str()
            .expect("disk revision")
            .to_owned()
    }

    async fn get(&self, path: &str) -> Value {
        self.request("GET", path, &Value::Null).await.json()
    }

    async fn generation(&self) -> Value {
        self.get("/api/v1/generations").await["generation"].clone()
    }

    async fn wait_generation(&self, what: &str, accept: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let generation = self.generation().await;
            if accept(&generation) {
                return generation;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {generation}"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn max_connections(&self) -> u64 {
        let listeners = self.get("/api/v1/listeners").await;
        listeners["listeners"][0]["maxConnections"]
            .as_u64()
            .expect("listener connection limit")
    }

    fn socket_mode(&self) -> u32 {
        fs::metadata(&self.socket)
            .expect("unix socket")
            .permissions()
            .mode()
            & 0o777
    }
}

fn required(generation: &Value) -> bool {
    generation["restartRequired"]["required"] == true
}

async fn one_reason_on_every_surface(supervised: bool) {
    let daemon = Daemon::start(supervised).await;
    let before = daemon.generation().await;
    assert_eq!(before["restartRequired"]["required"], false);
    assert_eq!(before["restartRequired"]["reasons"], json!([]));
    let active = before["activeRevision"]
        .as_str()
        .expect("active")
        .to_owned();

    let candidate = daemon.candidate(0o660, 7);
    daemon.write(&candidate);
    daemon.hang_up();
    let generation = daemon
        .wait_generation("the rest of the candidate to apply", |generation| {
            required(generation) && generation["activeRevision"] != active
        })
        .await;
    let applied = generation["activeRevision"].clone();
    assert_eq!(daemon.max_connections().await, 7);
    let restart = generation["restartRequired"].clone();
    let reason = restart["reasons"][0].clone();
    assert_eq!(
        restart["reasons"].as_array().map(Vec::len),
        Some(1),
        "{restart}"
    );
    assert_eq!(reason["code"], "unix_listener_mode");
    assert_eq!(reason["path"], "/config/listeners/live/bind/mode");
    assert!(
        reason["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("0600"))
    );
    assert!(restart["since"].as_u64().is_some_and(|since| since > 0));
    let config = daemon.get("/api/v1/config").await;
    assert_eq!(restart["candidateRevision"], config["candidateRevision"]);
    assert_ne!(restart["candidateRevision"], applied);

    assert_eq!(generation["degraded"], false, "{generation}");
    assert_eq!(generation["quarantinedRevision"], Value::Null);
    assert_eq!(generation["lastFailure"], Value::Null);
    assert_eq!(generation["lastRejection"], Value::Null);
    assert_eq!(
        daemon.get("/api/v1/status").await["restartRequired"],
        restart
    );
    assert_eq!(daemon.socket_mode(), 0o600);

    let reload = daemon
        .request(
            "POST",
            "/api/v1/generations/reload",
            &json!({"expectedActiveRevision": applied}),
        )
        .await;
    assert_eq!(reload.status, 202, "{}", reload.text());
    let reload = reload.json();
    assert_eq!(reload["outcome"], "startup_requested");
    assert_eq!(reload["candidateRevision"], applied);
    assert_eq!(reload["restartRequired"]["reasons"], restart["reasons"]);

    let body = json!({ "config": candidate });
    let validated = daemon
        .request("POST", "/api/v1/config/validate", &body)
        .await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let validated = validated.json();
    assert_eq!(validated["restartRequired"], true);
    assert_eq!(validated["restartReasons"], restart["reasons"]);
    let diagnostic = validated["diagnostics"]
        .as_array()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|d| d["code"] == "I_RESTART_REQUIRED")
        })
        .expect("restart diagnostic");
    assert_eq!(diagnostic["path"], reason["path"]);
    assert_eq!(diagnostic["message"], reason["summary"]);

    let after = daemon.generation().await;
    assert_eq!(after["degraded"], false, "{after}");
    assert_eq!(after["quarantinedRevision"], Value::Null);
    assert_eq!(after["failures"], 0);
    assert_eq!(after["restartRequired"], restart);
    let ready = http_request(daemon.management, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());

    daemon.write(&daemon.candidate(0o600, 7));
    daemon.hang_up();
    let cleared = daemon
        .wait_generation("the restart no longer required", |g| !required(g))
        .await;
    assert_eq!(cleared["restartRequired"]["reasons"], json!([]));
    assert_eq!(cleared["restartRequired"]["candidateRevision"], Value::Null);
    assert_eq!(cleared["activeRevision"], applied);

    let log = daemon.server.shutdown_output();
    assert!(log.contains("only a restart applies"), "{log}");
}

#[tokio::test]
async fn direct_restart_reasons_are_the_same_and_the_rest_applies() {
    one_reason_on_every_surface(false).await;
}

#[tokio::test]
async fn supervised_restart_reasons_are_the_same_and_the_rest_applies() {
    one_reason_on_every_surface(true).await;
}

#[tokio::test]
async fn a_configuration_only_the_direct_runtime_runs_waits_for_a_restart() {
    let daemon = Daemon::start(true).await;
    let active = daemon.generation().await["activeRevision"].clone();
    let mut candidate = daemon.active.clone();
    let template = candidate.listeners[0].clone();
    for index in 0..70 {
        let mut listener = template.clone();
        listener.name = format!("extra-{index}");
        listener.bind = ListenerBind::Socket {
            address: reserve_tcp_address(),
            ipv6_only: false,
        };
        candidate.listeners.push(listener);
    }
    daemon.write(&candidate);
    daemon.hang_up();

    let generation = daemon
        .wait_generation("the restart to be required", required)
        .await;

    let reason = &generation["restartRequired"]["reasons"][0];
    assert_eq!(reason["code"], "execution_mode");
    assert_eq!(reason["path"], "/config");
    assert_eq!(generation["activeRevision"], active);
    assert_eq!(generation["degraded"], false, "{generation}");
    assert_eq!(generation["lastRejection"], Value::Null);
    assert_eq!(generation["quarantinedRevision"], Value::Null);
    let ready = http_request(daemon.management, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 200, "{}", ready.text());
}

#[tokio::test]
async fn the_direct_runtime_keeps_its_registry_settings_and_applies_the_rest() {
    let daemon = Daemon::launch(false, true).await;
    let socket = daemon.socket.with_file_name("registry.sock");
    let mut candidate = daemon.candidate(0o600, 7);
    candidate.registry.as_mut().expect("registry").endpoints = vec![RegistryEndpoint::Unix {
        path: socket.clone(),
        mode: 0o660,
        role: RegistryRole::Read,
        pools: Vec::new(),
    }];
    let active = daemon.generation().await["activeRevision"].clone();
    daemon.write(&candidate);

    let generation = daemon
        .wait_generation("the rest of the candidate to apply", |generation| {
            required(generation) && generation["activeRevision"] != active
        })
        .await;

    let reason = &generation["restartRequired"]["reasons"][0];
    assert_eq!(reason["code"], "registry_settings");
    assert_eq!(reason["path"], "/config/registry");
    assert_eq!(daemon.max_connections().await, 7);
    assert!(
        !socket.exists(),
        "the registry endpoint waits for a restart"
    );
}
