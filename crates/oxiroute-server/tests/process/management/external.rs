use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use oxiroute_config::Stats;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::sleep;

use crate::http_support::http_request;
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::shared::{TOKEN, management_config};

const WAIT: Duration = Duration::from_secs(20);
const NEXT_TOKEN: &str = "5d41402abc4b2a76b9719d911017c592a3b6c2e5d0f8e4a1b7c9d3f2e6a8b0c4";
const ADMIN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const LAST_TOKEN: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const NEXT_ADMIN: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

struct Daemon {
    server: ServerProcess,
    management: SocketAddr,
    stats: SocketAddr,
    admin_token: PathBuf,
    _directory: TempDir,
}

fn replace_token(path: &Path, token: &str) {
    let staged = path.with_extension("next");
    fs::write(&staged, format!("{token}\n")).expect("stage token");
    fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .expect("token mode");
    fs::rename(&staged, path).expect("replace token");
}

impl Daemon {
    async fn start(supervised: bool) -> Self {
        let directory = TempDir::new().expect("external input directory");
        let admin_token = directory.path().join("admin.token");
        replace_token(&admin_token, ADMIN);
        let management = reserve_tcp_address();
        let stats = reserve_tcp_address();
        let mut config = management_config(management, None);
        config.stats = Some(Stats {
            binds: vec![stats],
            admin_token_file: Some(admin_token.clone()),
            pages: Vec::new(),
        });
        let validated = config.validate().expect("valid external input config");
        let mut server = if supervised {
            ServerProcess::start_supervised(&validated, TOKEN)
        } else {
            ServerProcess::start(&validated, Some(TOKEN))
        };
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(stats).await;
        Self {
            server,
            management,
            stats,
            admin_token,
            _directory: directory,
        }
    }

    fn management_token(&self) -> &Path {
        self.server.token_path.as_deref().expect("management token")
    }

    fn hang_up(&self) {
        let status = Command::new("kill")
            .arg("-HUP")
            .arg(self.server.pid().to_string())
            .status()
            .expect("signal the server");
        assert!(status.success());
    }

    async fn generations(&self, token: &str) -> (u16, Value) {
        let authorization = format!("Bearer {token}");
        let response = http_request(
            self.management,
            "GET",
            "/api/v1/generations",
            &[("Authorization", &authorization)],
            &[],
        )
        .await;
        let body = if response.status == 200 {
            response.json()
        } else {
            Value::Null
        };
        (response.status, body)
    }

    async fn reload(&self, token: &str, revision: &Value) -> u16 {
        let authorization = format!("Bearer {token}");
        let body = serde_json::to_vec(&serde_json::json!({ "expectedActiveRevision": revision }))
            .expect("reload body");
        http_request(
            self.management,
            "POST",
            "/api/v1/generations/reload",
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            &body,
        )
        .await
        .status
    }

    async fn stats_status(&self, token: &str) -> u16 {
        let authorization = format!("Bearer {token}");
        http_request(
            self.stats,
            "GET",
            "/stats",
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .status
    }

    async fn wait_until(&self, what: &str, mut done: impl AsyncFnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done().await {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn rewritten_token_files_take_effect_on_the_next_reload(supervised: bool) {
    let daemon = Daemon::start(supervised).await;
    let (status, before) = daemon.generations(TOKEN).await;
    assert_eq!(status, 200);
    assert_eq!(daemon.stats_status(ADMIN).await, 200);

    replace_token(daemon.management_token(), NEXT_TOKEN);
    daemon.hang_up();
    daemon
        .wait_until("the new management token", async || {
            daemon.generations(NEXT_TOKEN).await.0 == 200
        })
        .await;
    let (_, after) = daemon.generations(NEXT_TOKEN).await;
    assert_ne!(
        after["generation"]["activeRevision"],
        before["generation"]["activeRevision"]
    );
    assert_eq!(daemon.generations(TOKEN).await.0, 401);
    assert_eq!(after["generation"]["degraded"], false, "{after}");
    assert_eq!(after["generation"]["restartRequired"]["required"], false);

    replace_token(&daemon.admin_token, NEXT_ADMIN);
    daemon.hang_up();
    daemon
        .wait_until("the new stats admin token", async || {
            daemon.stats_status(NEXT_ADMIN).await == 200
        })
        .await;
    assert_eq!(daemon.stats_status(ADMIN).await, 401);

    let (_, last) = daemon.generations(NEXT_TOKEN).await;
    let last_revision = last["generation"]["activeRevision"].clone();
    assert_ne!(last_revision, after["generation"]["activeRevision"]);

    replace_token(daemon.management_token(), LAST_TOKEN);
    assert_eq!(daemon.reload(NEXT_TOKEN, &last_revision).await, 202);
    daemon
        .wait_until(
            "the management token a reload request picks up",
            async || daemon.generations(LAST_TOKEN).await.0 == 200,
        )
        .await;
    assert_eq!(daemon.generations(NEXT_TOKEN).await.0, 401);
}

#[tokio::test]
async fn direct_rewritten_token_files_take_effect_on_the_next_reload() {
    rewritten_token_files_take_effect_on_the_next_reload(false).await;
}

#[tokio::test]
async fn supervised_rewritten_token_files_take_effect_on_the_next_reload() {
    rewritten_token_files_take_effect_on_the_next_reload(true).await;
}
