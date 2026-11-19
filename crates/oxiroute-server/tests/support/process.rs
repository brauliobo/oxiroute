#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

use std::{
    fs::{self, File},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::{ConfigDraft, HttpDataPlane, Protocol, ValidatedConfig};
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

#[path = "accept_queue.rs"]
pub mod accept_queue;
#[path = "descendants.rs"]
pub mod descendants;
#[path = "ports.rs"]
pub mod ports;

/// A loopback address reserved for this test process alone; see [`ports`].
pub fn reserve_tcp_address() -> SocketAddr {
    ports::unused_address()
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
    stderr_path: PathBuf,
    pub config_path: PathBuf,
    pub token_path: Option<PathBuf>,
    direct: bool,
    binary: PathBuf,
    environment: Vec<(String, String)>,
    _directory: TempDir,
}

impl ServerProcess {
    pub fn start(config: &ValidatedConfig, token: Option<&str>) -> Self {
        Self::launch(config, token, true, &[])
    }

    /// [`Self::start`] with extra environment variables (the internal test hooks), which the supervised workers do not
    /// inherit: the process runs the direct runtime.
    #[allow(dead_code)]
    pub fn start_with_env(
        config: &ValidatedConfig,
        token: Option<&str>,
        environment: &[(&str, &str)],
    ) -> Self {
        let direct_runtime = [("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME", "1")];
        let environment = [&direct_runtime[..], environment].concat();
        Self::launch(config, token, true, &environment)
    }

    /// Starts the production topology: a master that replaces its worker on every accepted file.
    pub fn start_supervised(config: &ValidatedConfig, token: &str) -> Self {
        Self::launch(config, Some(token), false, &[])
    }

    /// [`Self::start_supervised`] with extra environment variables for the master.
    #[allow(dead_code)]
    pub fn start_supervised_with_env(
        config: &ValidatedConfig,
        token: &str,
        environment: &[(&str, &str)],
    ) -> Self {
        Self::launch(config, Some(token), false, environment)
    }

    /// Starts the production topology from the executable at `binary`, which the test may replace later.
    #[allow(dead_code)]
    pub fn start_supervised_binary(
        config: &ValidatedConfig,
        token: &str,
        binary: &Path,
        environment: &[(&str, &str)],
    ) -> Self {
        let authored = render_lua(&on_suite_data_plane(config)).expect("render process config");
        Self::launch_text(
            ("oxiroute.lua", &authored),
            Some(token),
            (false, environment),
            binary,
        )
    }

    /// Starts the direct runtime on authored text, kept byte for byte in a file named `file_name`.
    #[allow(dead_code)]
    pub fn start_authored(file_name: &str, authored: &str, token: &str) -> Self {
        Self::launch_text(
            (file_name, authored),
            Some(token),
            (true, &[]),
            Path::new(env!("CARGO_BIN_EXE_oxiroute")),
        )
    }

    fn launch(
        config: &ValidatedConfig,
        token: Option<&str>,
        direct: bool,
        environment: &[(&str, &str)],
    ) -> Self {
        let authored = render_lua(&on_suite_data_plane(config)).expect("render process config");
        Self::launch_text(
            ("oxiroute.lua", &authored),
            token,
            (direct, environment),
            Path::new(env!("CARGO_BIN_EXE_oxiroute")),
        )
    }

    fn launch_text(
        (file_name, authored): (&str, &str),
        token: Option<&str>,
        (direct, environment): (bool, &[(&str, &str)]),
        binary: &Path,
    ) -> Self {
        let directory = TempDir::new().expect("server process directory");
        let config_path = directory.path().join(file_name);
        fs::write(&config_path, authored).expect("write process config");
        let token_path = token.map(|token| write_token(directory.path(), token, 0o600));
        let stderr_path = directory.path().join("oxiroute.stderr");
        let stderr = File::create(&stderr_path).expect("server standard error file");
        let child = spawn_server(
            binary,
            (&config_path, token_path.as_deref()),
            (Stdio::null(), stderr.into()),
            (direct, environment),
        );
        Self {
            child,
            stderr_path,
            config_path,
            token_path,
            direct,
            binary: binary.to_owned(),
            environment: environment
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            _directory: directory,
        }
    }

    /// Stops the server and starts it again on the same files and with the same environment.
    #[allow(dead_code)]
    pub fn restart(&mut self) {
        self.stop_gracefully();
        let stderr = fs::OpenOptions::new()
            .append(true)
            .open(&self.stderr_path)
            .expect("server standard error file");
        let environment = self
            .environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        self.child = spawn_server(
            &self.binary,
            (&self.config_path, self.token_path.as_deref()),
            (Stdio::null(), stderr.into()),
            (self.direct, &environment),
        );
    }

    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// How the server ended, or `None` while it runs.
    #[allow(dead_code)]
    pub fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().expect("inspect server process")
    }

    pub async fn wait_for_tcp(&mut self, address: SocketAddr) {
        self.connect_when_bound(address).await;
    }

    /// Keeps the connection that proves the bind: a separate readiness probe would itself occupy a
    /// slot of a connection-limited listener until the server noticed it closed. The connect
    /// succeeds as soon as the server's listening socket exists, which is well before the server
    /// accepts: the connection returns once the server has accepted it.
    pub async fn connect_when_bound(&mut self, address: SocketAddr) -> TcpStream {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let stream = loop {
            self.assert_running();
            if let Ok(stream) = TcpStream::connect(address).await {
                break stream;
            }
            assert!(Instant::now() < deadline, "server did not bind {address}");
            sleep(RETRY_DELAY).await;
        };
        while accept_queue::pending_accepts(address).is_some_and(|pending| pending > 0) {
            self.assert_running();
            assert!(
                Instant::now() < deadline,
                "server did not accept on {address}: {}",
                self.stderr()
            );
            sleep(RETRY_DELAY).await;
        }
        stream
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

    #[allow(dead_code)]
    pub fn thread_names(&self) -> Vec<String> {
        let tasks = fs::read_dir(format!("/proc/{}/task", self.child.id()));
        let tasks = tasks.expect("the threads of the server");
        let names = tasks
            .flatten()
            .filter_map(|task| fs::read_to_string(task.path().join("comm")).ok());
        names.map(|name| name.trim_end().to_owned()).collect()
    }

    /// Returns once the server runs a thread whose name starts with `prefix`.
    #[allow(dead_code)]
    pub async fn wait_for_thread(&mut self, prefix: &str) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while !self
            .thread_names()
            .iter()
            .any(|name| name.starts_with(prefix))
        {
            self.assert_running();
            assert!(
                Instant::now() < deadline,
                "server did not start a thread named {prefix}"
            );
            sleep(RETRY_DELAY).await;
        }
    }

    pub fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("inspect server process") {
            panic!("server exited early with {status}: {}", self.stderr());
        }
    }

    /// Whether the server is still running or how it ended, with everything it wrote to standard error.
    #[allow(dead_code)]
    pub fn report(&mut self) -> String {
        let state = match self.child.try_wait().expect("inspect server process") {
            Some(status) => format!("the server ended with {status}"),
            None => "the server is still running".to_owned(),
        };
        let process =
            fs::read_to_string(format!("/proc/{}/status", self.child.id())).unwrap_or_default();
        let listening = fs::read_to_string("/proc/net/tcp").unwrap_or_default();
        let listening = listening
            .lines()
            .filter(|row| row.split_whitespace().nth(3) == Some("0A"));
        let listening = listening
            .filter_map(|row| row.split_whitespace().nth(1))
            .collect::<Vec<_>>();
        format!(
            "{state}; process:\n{process}\nlistening: {listening:?}\nstandard error:\n{}",
            self.stderr()
        )
    }

    /// Everything the server has written to standard error so far.
    pub fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr_path).expect("server standard error")
    }

    /// Returns what the server has written to standard error once it contains `text`.
    #[allow(dead_code)]
    pub async fn wait_for_stderr(&mut self, text: &str) -> String {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            self.assert_running();
            let written = self.stderr();
            if written.contains(text) {
                return written;
            }
            assert!(
                Instant::now() < deadline,
                "server did not write `{text}`: {written}"
            );
            sleep(RETRY_DELAY).await;
        }
    }

    pub fn shutdown(mut self) {
        self.kill();
    }

    /// Asks the server to stop and waits for it to end, however it ends.
    #[allow(dead_code)]
    pub fn terminate(mut self) {
        self.stop_gracefully();
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

    /// Stops the server and returns what it wrote to standard error.
    #[allow(dead_code)]
    pub fn shutdown_output(mut self) -> String {
        self.stop_gracefully();
        self.stderr()
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

    fn kill(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            descendants::kill_tree(self.child.id());
        }
        let _ = self.child.wait();
    }

    fn stop_gracefully(&mut self) {
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
        self.kill();
    }
}

const DATA_PLANE_ENV: &str = "OXIROUTE_TEST_HTTP_DATA_PLANE";

/// The HTTP data plane a whole suite runs on, from the test variable: `epoll`, `io_uring` or `tokio`. Omitted leaves each
/// configuration to its own choice, which defaults to the first available plane of the chain.
pub fn suite_data_plane() -> Option<HttpDataPlane> {
    match std::env::var(DATA_PLANE_ENV).ok()?.as_str() {
        "epoll" => Some(HttpDataPlane::Epoll),
        "io_uring" => Some(HttpDataPlane::IoUring),
        "tokio" => Some(HttpDataPlane::Tokio),
        other => panic!("{DATA_PLANE_ENV} is `epoll`, `io_uring` or `tokio`, not `{other}`"),
    }
}

/// Whether the plain HTTP listeners of the configurations of the suite are served by the epoll data plane.
#[allow(dead_code)]
pub fn serves_on_epoll() -> bool {
    suite_data_plane() != Some(HttpDataPlane::Tokio) && oxiroute_epoll_h1::supported()
}

fn takes_a_data_plane(draft: &ConfigDraft, plane: HttpDataPlane) -> bool {
    draft.fast_lane_enabled()
        && !(plane == HttpDataPlane::IoUring && draft.aged_per_worker())
        && draft
            .listeners
            .iter()
            .any(|listener| listener.protocol == Protocol::Http)
}

fn on_suite_data_plane(config: &ValidatedConfig) -> ValidatedConfig {
    let Some(plane) =
        suite_data_plane().filter(|plane| takes_a_data_plane(config.as_draft(), *plane))
    else {
        return config.clone();
    };
    let mut draft = config.as_draft().clone();
    draft.http_data_plane.get_or_insert(plane);
    draft
        .validate()
        .expect("the configuration validates on the suite's data plane")
}

pub fn write_config(path: &Path, config: &ValidatedConfig) {
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        render_lua(&on_suite_data_plane(config)).expect("render process config"),
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
    let piped = (Stdio::piped(), Stdio::piped());
    let binary = Path::new(env!("CARGO_BIN_EXE_oxiroute"));
    let mut child = spawn_server(binary, (config_path, token_path), piped, (true, &[]));
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

fn spawn_server(
    binary: &Path,
    (config_path, token_path): (&Path, Option<&Path>),
    (stdout, stderr): (Stdio, Stdio),
    (direct, environment): (bool, &[(&str, &str)]),
) -> Child {
    let mut command = Command::new(binary);
    command
        .arg(config_path)
        .env_remove("OXIROUTE_MANAGEMENT_TOKEN_FILE")
        .env_remove("RUST_LOG")
        .envs(environment.iter().copied())
        .stdout(stdout)
        .stderr(stderr);
    if direct && token_path.is_some() {
        command.env("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME", "1");
    }
    if !direct {
        command.env(
            "OXIROUTE_INTERNAL_TEST_WORKER_LAUNCHER",
            env!("CARGO_BIN_EXE_oxiroute-supervisor-launcher-fixture"),
        );
    }
    if let Some(token_path) = token_path {
        command.env("OXIROUTE_MANAGEMENT_TOKEN_FILE", token_path);
    }
    command.spawn().expect("spawn built OxiRoute server")
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
