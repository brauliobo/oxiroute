use std::{
    fs,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, ListenerBind, Management, Registry};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{net::TcpStream, time::sleep};

use crate::http_support::{HttpResponse, http_request};
use crate::process_support::{ServerProcess, reserve_tcp_address, write_config};
use crate::shared::{TOKEN, rtmp_listener_config};

pub(super) const WAIT: Duration = Duration::from_secs(20);

pub(super) struct Daemon {
    pub(super) server: ServerProcess,
    pub(super) management: SocketAddr,
    pub(super) listener: SocketAddr,
    pub(super) path: PathBuf,
    pub(super) active: ConfigDraft,
    _directory: TempDir,
}

impl Daemon {
    pub(super) async fn start(supervised: bool) -> Self {
        Self::launch(supervised, false, &[]).await
    }

    pub(super) async fn launch(
        supervised: bool,
        registry: bool,
        environment: &[(&str, &str)],
    ) -> Self {
        let directory = TempDir::new().expect("restart case directory");
        let path = directory.path().to_path_buf();
        let management = reserve_tcp_address();
        let port = reserve_tcp_address().port();
        let listener = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut active = rtmp_listener_config(ListenerBind::Socket {
            address: SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
            ipv6_only: false,
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
            ServerProcess::start_supervised_with_env(&validated, TOKEN, environment)
        } else {
            ServerProcess::start_with_env(&validated, Some(TOKEN), environment)
        };
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(listener).await;
        Self {
            server,
            management,
            listener,
            path,
            active,
            _directory: directory,
        }
    }

    pub(super) fn candidate(&self, ipv6_only: bool, max_connections: u64) -> ConfigDraft {
        let mut candidate = self.active.clone();
        candidate.listeners[0].bind = ListenerBind::Socket {
            address: SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.listener.port())),
            ipv6_only,
        };
        candidate.listeners[0].max_connections = Some(max_connections);
        candidate
    }

    pub(super) fn write(&self, config: &ConfigDraft) {
        write_config(
            &self.server.config_path,
            &config.clone().validate().expect("valid candidate"),
        );
    }

    pub(super) fn hang_up(&self) {
        let status = Command::new("kill")
            .arg("-HUP")
            .arg(self.server.pid().to_string())
            .status()
            .expect("signal the server");
        assert!(status.success());
    }

    pub(super) async fn request(&self, method: &str, path: &str, body: &Value) -> HttpResponse {
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

    pub(super) async fn disk_revision(&self) -> String {
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

    pub(super) async fn get(&self, path: &str) -> Value {
        self.request("GET", path, &Value::Null).await.json()
    }

    pub(super) async fn generation(&self) -> Value {
        self.get("/api/v1/generations").await["generation"].clone()
    }

    pub(super) async fn wait_generation(
        &self,
        what: &str,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
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

    pub(super) async fn restart_gauge(&self) -> String {
        let metrics = http_request(self.management, "GET", "/metrics", &[], &[]).await;
        metrics
            .text()
            .lines()
            .find(|line| line.starts_with("oxiroute_restart_required "))
            .expect("restart gauge")
            .to_owned()
    }

    pub(super) async fn max_connections(&self) -> u64 {
        let listeners = self.get("/api/v1/listeners").await;
        listeners["listeners"][0]["maxConnections"]
            .as_u64()
            .expect("listener connection limit")
    }

    pub(super) async fn accepts_ipv4(&self) -> bool {
        TcpStream::connect(self.listener).await.is_ok()
    }
}
