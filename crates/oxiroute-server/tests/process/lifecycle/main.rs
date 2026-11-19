#![cfg(target_os = "linux")]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "../../support/config.rs"]
mod config_support;
#[path = "../../support/fixtures.rs"]
mod fixture_support;
#[path = "../../support/http.rs"]
mod http_support;
#[path = "../../support/process.rs"]
mod process_support;
#[path = "../../support/relay.rs"]
mod relay_support;
#[path = "../../support/mod.rs"]
mod wire_support;

use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, Retirement};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::sleep,
};

use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use relay_support::{WIRE_TIMEOUT, assert_echo, start_echo_upstream, tcp_config};

mod build_id;
mod retired;
mod upgrade;

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
static PROCESS_TEST_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

async fn process_test_guard() -> OwnedSemaphorePermit {
    PROCESS_TEST_GATE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("process test gate remains open")
}

/// A supervised daemon started from its own copy of the executable, which a test may replace.
struct Daemon {
    server: ServerProcess,
    management: SocketAddr,
    listener: SocketAddr,
    config: ConfigDraft,
    upstream: JoinHandle<()>,
    binary: PathBuf,
    sequence: u64,
    successors: Vec<Successor>,
    directory: TempDir,
}

/// A master the daemon handed over to, ended with the test.
struct Successor(u32);

impl Drop for Successor {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.0.to_string())
            .status();
        let deadline = Instant::now() + WIRE_TIMEOUT;
        while fs::metadata(format!("/proc/{}", self.0)).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Daemon {
    async fn start(retirement: Retirement) -> Self {
        Self::start_in_environment(retirement, &[("INVOCATION_ID", "")]).await
    }

    async fn start_in_environment(retirement: Retirement, environment: &[(&str, &str)]) -> Self {
        let directory = TempDir::new().expect("daemon binary directory");
        let binary = directory.path().join("oxiroute");
        fs::copy(env!("CARGO_BIN_EXE_oxiroute"), &binary).expect("copy the executable");
        let (upstream_address, upstream) = start_echo_upstream().await;
        let management = reserve_tcp_address();
        let listener = reserve_tcp_address();
        let mut config = tcp_config(management, listener, upstream_address, retirement);
        config.listeners[0].max_connections = Some(1_024);
        let validated = config.clone().validate().expect("lifecycle config");
        let mut server = ServerProcess::start_supervised_binary(
            &validated,
            TOKEN,
            &binary,
            &[&[("RUST_LOG", "info")], environment].concat(),
        );
        server.wait_for_tcp(management).await;
        server.wait_for_tcp(listener).await;
        Self {
            server,
            management,
            listener,
            config,
            upstream,
            binary,
            sequence: 0,
            successors: Vec::new(),
            directory,
        }
    }

    async fn get(&self, path: &str) -> Value {
        let authorization = format!("Bearer {TOKEN}");
        http_request(
            self.management,
            "GET",
            path,
            &[("Authorization", &authorization)],
            &[],
        )
        .await
        .json()
    }

    async fn generation(&self) -> Value {
        self.get("/api/v1/generations").await["generation"].clone()
    }

    async fn status(&self) -> Value {
        self.get("/api/v1/status").await
    }

    fn write_next_revision(&mut self) {
        self.sequence += 1;
        self.config.max_connections = Some(1_000 + self.sequence);
        write_config(
            &self.server.config_path,
            &self
                .config
                .clone()
                .validate()
                .expect("next lifecycle config"),
        );
    }

    fn signal_master(&self, signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(self.server.pid().to_string())
            .status()
            .expect("signal the master");
        assert!(status.success(), "signalling the master failed");
    }

    async fn open_relay(&self) -> TcpStream {
        let mut stream = TcpStream::connect(self.listener)
            .await
            .expect("relay connection");
        assert_echo(&mut stream, b"hello").await;
        stream
    }

    fn stop(self) {
        self.upstream.abort();
        self.server.shutdown();
        drop(self.successors);
    }

    fn install_executable(&self, image: &[u8]) {
        let staged = self.directory.path().join("oxiroute.staged");
        fs::write(&staged, image).expect("write the executable");
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .expect("make the executable runnable");
        fs::rename(&staged, &self.binary).expect("replace the executable");
    }

    /// Runs `oxiroute upgrade` against the daemon, off the test's runtime so that traffic keeps flowing.
    async fn upgrade(&self, arguments: &[&str]) -> Output {
        let config = self.server.config_path.clone();
        let arguments = arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_oxiroute"))
                .arg("upgrade")
                .arg("--config")
                .arg(config)
                .args(arguments)
                .output()
                .expect("run oxiroute upgrade")
        })
        .await
        .expect("the upgrade command ends")
    }

    async fn wait_for_master_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WIRE_TIMEOUT;
        loop {
            if let Some(status) = self.server.exit_status() {
                return status;
            }
            assert!(Instant::now() < deadline, "the master did not end");
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn adopt_successor(&mut self, output: &Output) -> u32 {
        let text = String::from_utf8_lossy(&output.stdout);
        let pid = text
            .split("successor master ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|pid| pid.parse().ok())
            .unwrap_or_else(|| panic!("the upgrade names its successor: {text}"));
        self.successors.push(Successor(pid));
        pid
    }
}

/// The executable with the build id its linker recorded altered, so that it names another build.
fn another_build_of(executable: &Path, build_id: &str, change: u8) -> Vec<u8> {
    let recorded = build_id.strip_prefix("gnu:").expect("a GNU build id");
    let id = (0..recorded.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&recorded[at..at + 2], 16).expect("hex build id"))
        .collect::<Vec<_>>();
    let mut image = fs::read(executable).expect("read the executable");
    let at = image
        .windows(id.len())
        .position(|window| window == id)
        .expect("the build id is recorded in the image");
    image[at] ^= change;
    image
}

async fn wait_until<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WIRE_TIMEOUT;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25)).await;
    }
}
