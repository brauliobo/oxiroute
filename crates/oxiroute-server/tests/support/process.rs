#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::{
    fs,
    io::Read,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::ValidatedConfig;
use tempfile::TempDir;
use tokio::{net::TcpStream, time::sleep};

use crate::fixture_support::write_file_with_mode;

fn render_lua(config: &ValidatedConfig) -> Result<String, String> {
    oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, config)
        .map_err(|error| error.to_string())
}

pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_millis(10);
static UI_BUILD: OnceLock<()> = OnceLock::new();

pub fn reserve_tcp_address() -> SocketAddr {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve TCP address")
        .local_addr()
        .expect("reserved TCP address")
}

pub fn build_ui() -> PathBuf {
    UI_BUILD.get_or_init(|| {
        let ui = workspace_root().join("ui");
        let output = Command::new("pnpm")
            .arg("build")
            .current_dir(&ui)
            .output()
            .expect("run the existing UI build");
        assert!(
            output.status.success(),
            "UI build failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    });
    workspace_root().join("ui/dist")
}

pub struct ServerProcess {
    child: Child,
    pub config_path: PathBuf,
    pub token_path: Option<PathBuf>,
    _directory: TempDir,
}

impl ServerProcess {
    pub fn start(config: &ValidatedConfig, token: Option<&str>) -> Self {
        let directory = TempDir::new().expect("server process directory");
        let config_path = directory.path().join("oxiroute.lua");
        write_config(&config_path, config);
        let token_path = token.map(|token| write_token(directory.path(), token, 0o600));
        let child = spawn_server(&config_path, token_path.as_deref());
        Self {
            child,
            config_path,
            token_path,
            _directory: directory,
        }
    }

    pub async fn wait_for_tcp(&mut self, address: SocketAddr) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            self.assert_running();
            if TcpStream::connect(address).await.is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "server did not bind {address}");
            sleep(RETRY_DELAY).await;
        }
    }

    #[cfg(unix)]
    pub async fn wait_for_unix(&mut self, path: &Path) {
        use tokio::net::UnixStream;

        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            self.assert_running();
            if UnixStream::connect(path).await.is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "server did not bind Unix socket");
            sleep(RETRY_DELAY).await;
        }
    }

    fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("inspect server process") {
            let stderr = read_pipe(self.child.stderr.take());
            panic!("server exited early with {status}: {stderr}");
        }
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    #[allow(dead_code)]
    pub fn shutdown_gracefully(mut self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("signal server process");
        assert!(status.success(), "failed to signal server process");
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while self
            .child
            .try_wait()
            .expect("inspect server process")
            .is_none()
        {
            assert!(
                Instant::now() < deadline,
                "server did not shut down gracefully"
            );
            thread::sleep(RETRY_DELAY);
        }
        assert!(self.child.wait().expect("server exit").success());
    }

    pub fn wait_for_exit(mut self) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while self
            .child
            .try_wait()
            .expect("inspect server process")
            .is_none()
        {
            assert!(Instant::now() < deadline, "server did not exit");
            thread::sleep(RETRY_DELAY);
        }
        assert!(self.child.wait().expect("server exit").success());
    }

    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(self.child.id().to_string())
                .status();
            let deadline = Instant::now() + PROCESS_TIMEOUT;
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(RETRY_DELAY);
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn write_config(path: &Path, config: &ValidatedConfig) {
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        render_lua(config).expect("render process config"),
    )
    .expect("write process config");
    fs::rename(temporary, path).expect("install process config");
}

pub fn write_token(directory: &Path, token: &str, mode: u32) -> PathBuf {
    write_file_with_mode(
        directory,
        "management.token",
        format!("{token}\n").as_bytes(),
        mode,
    )
}

pub fn run_to_failure(config_path: &Path, token_path: Option<&Path>) -> Output {
    let mut child = spawn_server(config_path, token_path);
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if child.try_wait().expect("inspect failing server").is_some() {
            return child
                .wait_with_output()
                .expect("collect failing server output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("collect timed-out server output");
            panic!(
                "server unexpectedly remained running:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn spawn_server(config_path: &Path, token_path: Option<&Path>) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oxiroute"));
    command
        .arg(config_path)
        .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if token_path.is_some() {
        command.env("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME", "1");
    }
    if let Some(token_path) = token_path {
        command.env("OXIROUTE_MANAGEMENT_TOKEN_FILE", token_path);
    }
    command.spawn().expect("spawn built OxiRoute server")
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let mut output = String::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_string(&mut output);
    }
    output
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

pub fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
