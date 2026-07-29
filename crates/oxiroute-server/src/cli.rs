use std::{
    error::Error,
    ffi::OsString,
    fmt::{self, Write as _},
    fs::File,
    io::{self, BufRead, BufReader, Read as _, Write as _},
    net::{IpAddr, TcpStream, ToSocketAddrs as _},
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::Duration,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use oxiroute_config_source::{ConfigFormat, render_config};
use oxiroute_import::{Diagnostic, Severity};
use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{
    GenerationManager,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};

#[cfg(test)]
use std::io::Cursor;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9900";
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 512;
const EXIT_LOCAL: u8 = 3;
const EXIT_CONNECT: u8 = 4;
const EXIT_AUTH: u8 = 5;
const EXIT_NOT_FOUND: u8 = 6;
const EXIT_CONFLICT: u8 = 7;
const EXIT_REMOTE: u8 = 8;
const EXIT_UNSUPPORTED: u8 = 9;
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "oxiroute", version = BUILD_VERSION)]
pub struct Cli {
    #[arg(long, env = "OXIROUTE_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    endpoint: String,
    #[arg(long, env = "OXIROUTE_MANAGEMENT_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = Output::Table)]
    output: Output,
    #[arg(long)]
    quiet: bool,
    #[command(subcommand)]
    command: Option<Command>,

    /// Shorthand for `serve CONFIG` when no subcommand is present.
    #[arg(value_name = "CONFIG")]
    legacy_config: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Output {
    Json,
    Table,
    Plain,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the proxy and reconcile changes to its canonical configuration.
    Serve {
        #[arg(value_name = "CONFIG", default_value = "oxiroute.kdl")]
        config: PathBuf,
    },
    Status,
    Ready,
    Metrics,
    Topology,
    Monitoring,
    Drain,
    Shutdown,
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    Listener {
        #[command(subcommand)]
        command: ListenerCommand,
    },
    Generation {
        #[command(subcommand)]
        command: GenerationCommand,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Tls {
        #[command(subcommand)]
        command: TlsCommand,
    },
    Rtmp {
        #[command(subcommand)]
        command: RtmpCommand,
    },
    /// Import a native configuration.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Print the build version.
    Version,
}

#[derive(Subcommand)]
pub enum ImportCommand {
    Nginx {
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        #[arg(long, value_name = "DIRECTORY", default_value = "/")]
        root_prefix: PathBuf,
        /// Preserve native local-time recording suffixes with this IANA timezone.
        #[arg(long, value_name = "IANA_TIMEZONE")]
        host_timezone: Option<String>,
        /// Explicitly migrate omitted nginx combined logs to `OxiRoute` structured JSONL.
        #[arg(long, value_name = "FILE")]
        default_access_log_file: Option<PathBuf>,
        /// Replace one unique native recording root with this no-symlink canonical path.
        #[arg(long, value_name = "DIRECTORY")]
        recording_root: Option<PathBuf>,
        /// Preserve nginx's branded default static 404 body and response header.
        #[arg(long, value_name = "SERVER_TOKEN")]
        default_error_server: Option<String>,
        /// Shift imported IP socket listener ports for side-by-side validation.
        #[arg(long, value_name = "PORTS", value_parser = clap::value_parser!(u16).range(1..))]
        shadow_port_offset: Option<u16>,
        /// Render preview output in this canonical configuration format.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        #[arg(long, value_enum, default_value_t = ImportOutput::Report)]
        output: ImportOutput,
    },
    Haproxy {
        #[arg(value_name = "CONFIG", required = true)]
        configs: Vec<PathBuf>,
        /// Expand `${NODE_IP}` using this exact host address.
        #[arg(long, value_name = "IP")]
        node_ip: Option<IpAddr>,
        /// Treat `defined(GPU1)` conditions as true.
        #[arg(long, requires = "node_ip")]
        gpu1_defined: bool,
        /// Shift imported IP socket listener ports for side-by-side validation.
        #[arg(long, value_name = "PORTS", value_parser = clap::value_parser!(u16).range(1..))]
        shadow_port_offset: Option<u16>,
        /// Render preview output in this canonical configuration format.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        #[arg(long, value_enum, default_value_t = ImportOutput::Report)]
        output: ImportOutput,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ImportOutput {
    #[default]
    Report,
    Preview,
}

#[derive(Subcommand)]
pub enum EventsCommand {
    List {
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    Follow {
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        #[arg(long, default_value_t = 1_000)]
        interval_ms: u64,
    },
}

#[derive(Subcommand)]
pub enum ServerCommand {
    List,
    Show(ServerTarget),
    Ready(ServerTarget),
    Drain(ServerTarget),
    Maintenance(ServerTarget),
    SetHealth {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_enum)]
        health: HealthValue,
    },
    Check {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_enum)]
        action: Toggle,
    },
    MaxConnections {
        #[command(subcommand)]
        command: MaxConnectionsCommand,
    },
    RefreshDns(ServerTarget),
}

#[derive(Args)]
pub struct ServerTarget {
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pools: Vec<String>,
    #[arg(value_name = "SERVER", value_parser = nonempty_target)]
    server: String,
}

#[derive(Subcommand)]
pub enum MaxConnectionsCommand {
    Set {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        limit: u64,
    },
    Reset(ServerTarget),
}

#[derive(Clone, Copy, ValueEnum)]
pub enum HealthValue {
    Auto,
    Up,
    Down,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Toggle {
    Enable,
    Disable,
}

#[derive(Subcommand)]
pub enum PoolCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        pool: String,
    },
    Ready {
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
    },
    Drain {
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum ListenerCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        listener: String,
    },
    Ready {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    Drain {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    Maintenance {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum GenerationCommand {
    Status,
    Reload,
    Rollback,
    Drain {
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=300_000))]
        timeout_ms: u64,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    Check {
        config: PathBuf,
    },
    /// Compose finalized canonical configurations in the supplied order.
    Compose {
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        #[arg(value_name = "CONFIG", required = true, num_args = 1..)]
        configs: Vec<PathBuf>,
    },
    Get,
    Validate {
        file: PathBuf,
    },
    Apply {
        file: PathBuf,
    },
    Diff {
        file: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ComposeFormat {
    #[default]
    Kdl,
    Lua,
    Uci,
    Hocon,
}

impl From<ComposeFormat> for ConfigFormat {
    fn from(format: ComposeFormat) -> Self {
        match format {
            ComposeFormat::Kdl => Self::Kdl,
            ComposeFormat::Lua => Self::Lua,
            ComposeFormat::Uci => Self::Uci,
            ComposeFormat::Hocon => Self::Hocon,
        }
    }
}

#[derive(Subcommand)]
pub enum TlsCommand {
    List,
    Reconcile {
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RtmpCommand {
    Stream {
        #[command(subcommand)]
        command: StreamCommand,
    },
    Recorder {
        #[command(subcommand)]
        command: RecorderCommand,
    },
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
    },
}

#[derive(Subcommand)]
pub enum StreamCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
    Disconnect {
        #[arg(value_parser = nonempty_target)]
        stream: String,
    },
}

#[derive(Subcommand)]
pub enum RecorderCommand {
    Start {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
    Stop {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        recorder: String,
    },
}

#[derive(Subcommand)]
pub enum RelayCommand {
    Reconnect {
        #[arg(value_parser = nonempty_target)]
        stream: String,
        #[arg(value_parser = nonempty_target)]
        relay: String,
    },
}

fn nonempty_target(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("target must not be empty".into())
    } else {
        Ok(value.to_owned())
    }
}

impl Cli {
    #[must_use]
    pub fn parse_process() -> Self {
        Self::normalize(Self::parse())
    }

    /// Parses an injected argument sequence.
    ///
    /// # Errors
    ///
    /// Returns clap's structured parse error for an invalid command line.
    pub fn try_parse_process_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(arguments).map(Self::normalize)
    }

    fn normalize(mut cli: Self) -> Self {
        if cli.command.is_none() {
            cli.command = Some(Command::Serve {
                config: cli
                    .legacy_config
                    .take()
                    .unwrap_or_else(|| PathBuf::from("oxiroute.kdl")),
            });
        }
        cli
    }

    #[must_use]
    /// Returns the command resolved by process or injected-argument parsing.
    ///
    /// # Panics
    ///
    /// Panics only if a `Cli` value bypasses the private parser normalization invariant.
    pub fn command(&self) -> &Command {
        self.command
            .as_ref()
            .expect("CLI parsing always resolves a command")
    }

    #[must_use]
    pub fn execute_management(&self) -> ExitCode {
        match run(self) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                if !self.quiet {
                    eprintln!("oxiroute: {error}");
                }
                ExitCode::from(error.exit_code())
            }
        }
    }
}

/// Executes a non-serving local command and returns its stdout body.
///
/// # Errors
///
/// Returns a redacted validation or import failure. A report containing diagnostics is successful;
/// preview output requires a fully finalized canonical configuration.
pub fn execute_offline(command: &Command) -> Result<Option<String>, Box<dyn Error>> {
    match command {
        Command::Version => Ok(Some(format!("{BUILD_VERSION}\n"))),
        Command::Config {
            command: ConfigCommand::Check { config },
        } => {
            check_config(config)?;
            Ok(Some(
                "configuration is valid and runtime-preparable\n".into(),
            ))
        }
        Command::Config {
            command: ConfigCommand::Compose { format, configs },
        } => Ok(Some(compose_config_files(configs, (*format).into())?)),
        Command::Import {
            command:
                ImportCommand::Nginx {
                    config,
                    root_prefix,
                    host_timezone,
                    default_access_log_file,
                    recording_root,
                    default_error_server,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_nginx(
            config,
            root_prefix,
            host_timezone.as_deref(),
            default_access_log_file.as_deref(),
            recording_root.as_deref(),
            default_error_server.as_deref(),
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        Command::Import {
            command:
                ImportCommand::Haproxy {
                    configs,
                    node_ip,
                    gpu1_defined,
                    shadow_port_offset,
                    format,
                    output,
                },
        } => Ok(Some(import_haproxy(
            configs,
            *node_ip,
            *gpu1_defined,
            *shadow_port_offset,
            (*format).into(),
            *output,
        )?)),
        _ => Ok(None),
    }
}

fn check_config(path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(path)?;
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        return Err("canonical configuration was rejected".into());
    };
    GenerationManager::new()
        .validate_candidate(*document)
        .map_err(|error| {
            format!("configuration cannot be prepared as a complete runtime generation: {error}")
        })?;
    Ok(())
}

fn compose_config_files(paths: &[PathBuf], format: ConfigFormat) -> Result<String, Box<dyn Error>> {
    let mut configs = Vec::with_capacity(paths.len());
    for path in paths {
        let coordinator = CanonicalConfigCoordinator::new(path)?;
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            return Err(
                format!("canonical configuration `{}` was rejected", path.display()).into(),
            );
        };
        configs.push(document.normalized_config.clone());
    }
    let composed = oxiroute_config::compose_configs(&configs)?;
    Ok(render_config(format, &composed)?)
}

#[expect(
    clippy::too_many_arguments,
    reason = "offline nginx import inputs map directly to explicit operator overlays"
)]
fn import_nginx(
    path: &Path,
    root_prefix: &Path,
    host_timezone: Option<&str>,
    default_access_log_file: Option<&Path>,
    recording_root: Option<&Path>,
    default_error_server: Option<&str>,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let options = oxiroute_import::nginx::NginxImportOptions {
        host_timezones: host_timezone
            .map(
                |timezone| oxiroute_import::nginx::NginxHostTimezoneOverlay {
                    timezone: timezone.into(),
                },
            )
            .into_iter()
            .collect(),
        default_access_log: default_access_log_file.map(|path| {
            oxiroute_import::nginx::NginxDefaultAccessLogOverlay {
                path: path.to_path_buf(),
            }
        }),
        recording_root: recording_root.map(|path| {
            oxiroute_import::nginx::NginxRecordingRootOverlay {
                path: path.to_path_buf(),
            }
        }),
        default_error_page: default_error_server.map(|server| {
            oxiroute_import::nginx::NginxDefaultErrorPageOverlay {
                server: server.to_owned(),
            }
        }),
        ..oxiroute_import::nginx::NginxImportOptions::default()
    };
    let report = oxiroute_import::nginx::import_root_with_options(path, root_prefix, &options);
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.candidate.config.as_ref(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            let mut result = report_header("nginx", &report.diagnostics);
            writeln!(result, "finalized: {}", report.candidate.config.is_some())?;
            writeln!(
                result,
                "deployment requirements: {}",
                report.candidate.deployment_requirements.len()
            )?;
            writeln!(
                result,
                "activation requirements: {}",
                report.candidate.activation_requirements.len()
            )?;
            Ok(result)
        }
    }
}

fn import_haproxy(
    paths: &[PathBuf],
    node_ip: Option<IpAddr>,
    gpu1_defined: bool,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
    output: ImportOutput,
) -> Result<String, Box<dyn Error>> {
    let report = node_ip.map_or_else(
        || oxiroute_import::haproxy::import_roots(paths),
        |node_ip| {
            oxiroute_import::haproxy::import_roots_with_environment(
                paths,
                oxiroute_import::haproxy::PreprocessingEnvironment {
                    node_ip,
                    gpu1_defined,
                },
            )
        },
    );
    match output {
        ImportOutput::Preview => preview_with_shadow_listener_offset(
            report.value().config.as_ref(),
            shadow_port_offset,
            format,
        ),
        ImportOutput::Report => {
            if shadow_port_offset.is_some() {
                return Err("--shadow-port-offset requires --output preview".into());
            }
            let candidate = report.value();
            let mut result = report_header("haproxy", report.diagnostics());
            writeln!(result, "finalized: {}", candidate.config.is_some())?;
            writeln!(
                result,
                "deployment requirements: {}",
                candidate.deployment_requirements.len()
            )?;
            writeln!(
                result,
                "activation requirements: {}",
                candidate.activation_requirements.len()
            )?;
            Ok(result)
        }
    }
}

fn preview_with_shadow_listener_offset(
    config: Option<&oxiroute_config::Config>,
    shadow_port_offset: Option<u16>,
    format: ConfigFormat,
) -> Result<String, Box<dyn Error>> {
    let Some(offset) = shadow_port_offset else {
        return preview(config, format);
    };
    let mut config = config
        .ok_or("native configuration did not produce an activatable candidate")?
        .clone();
    for listener in &mut config.listeners {
        let oxiroute_config::ListenerBind::Socket { address } = &mut listener.bind else {
            continue;
        };
        address.set_port(
            address
                .port()
                .checked_add(offset)
                .ok_or("shadow listener port offset exceeds 65535")?,
        );
    }
    oxiroute_config::validate_config(&mut config)
        .map_err(|_| "shadow listener configuration is not valid")?;
    Ok(render_config(format, &config)?)
}

fn preview(
    config: Option<&oxiroute_config::Config>,
    format: ConfigFormat,
) -> Result<String, Box<dyn Error>> {
    let config = config.ok_or("native configuration did not produce an activatable candidate")?;
    Ok(render_config(format, config)?)
}

fn report_header(kind: &str, diagnostics: &[Diagnostic]) -> String {
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Error)
        .count();
    let warnings = diagnostics.len().saturating_sub(errors);
    let mut result = format!("importer: {kind}\nerrors: {errors}\nwarnings: {warnings}\n");
    for diagnostic in diagnostics {
        let _ = writeln!(
            result,
            "{} {:?}: {}",
            diagnostic.code().as_str(),
            diagnostic.severity(),
            diagnostic.message()
        );
    }
    result
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let endpoint = Endpoint::parse(&cli.endpoint)?;
    let token = cli.token_file.as_deref().map(read_token).transpose()?;
    let client = Client { endpoint, token };
    if execute(cli, &client)? == FollowOutcome::Follow {
        follow_events(cli, &client)?;
    }
    Ok(())
}

#[derive(Eq, PartialEq)]
enum FollowOutcome {
    Complete,
    Follow,
}

#[allow(clippy::too_many_lines)]
fn execute(cli: &Cli, client: &Client) -> Result<FollowOutcome, CliError> {
    let (response, selection) = match cli.command() {
        Command::Status => (client.get("/api/v1/status", true)?, None),
        Command::Ready => (client.get("/ready", false)?, None),
        Command::Metrics => (client.get("/metrics", false)?, None),
        Command::Topology => (client.get("/api/v1/topology", true)?, None),
        Command::Monitoring => (client.get("/api/v1/monitoring", true)?, None),
        Command::Drain => (
            client.mutation_json("POST", "/api/v1/process/drain", &json!({}))?,
            None,
        ),
        Command::Shutdown => (
            client.mutation_json("POST", "/api/v1/process/shutdown", &json!({}))?,
            None,
        ),
        Command::Events {
            command: EventsCommand::List { after, limit },
        } => (
            client.get(&format!("/api/v1/events?after={after}&limit={limit}"), true)?,
            None,
        ),
        Command::Events {
            command: EventsCommand::Follow { .. },
        } => return Ok(FollowOutcome::Follow),
        Command::Server {
            command: ServerCommand::List,
        } => (client.get("/api/v1/servers", true)?, None),
        Command::Server {
            command: ServerCommand::Show(target),
        } => (
            client.get("/api/v1/servers", true)?,
            Some(Selection::Server(target)),
        ),
        Command::Server {
            command: ServerCommand::Ready(target),
        } => server_state(client, target, "ready")?,
        Command::Server {
            command: ServerCommand::Drain(target),
        } => server_state(client, target, "drain")?,
        Command::Server {
            command: ServerCommand::Maintenance(target),
        } => server_state(client, target, "maintenance")?,
        Command::Server {
            command: ServerCommand::SetHealth { target, health },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/health-override",
                &server_body(target, Some(("health", health.as_str()))),
            )?,
            None,
        ),
        Command::Server {
            command: ServerCommand::Check { target, action },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/checks",
                &server_bool_body(target, "enabled", matches!(action, Toggle::Enable)),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::MaxConnections {
                    command: MaxConnectionsCommand::Set { target, limit },
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/max-connections",
                &server_capacity_body(target, Some(*limit)),
            )?,
            None,
        ),
        Command::Server {
            command:
                ServerCommand::MaxConnections {
                    command: MaxConnectionsCommand::Reset(target),
                },
        } => (
            client.mutation_json(
                "PUT",
                "/api/v1/servers/max-connections",
                &server_capacity_body(target, None),
            )?,
            None,
        ),
        Command::Server {
            command: ServerCommand::RefreshDns(target),
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/servers/refresh-dns",
                &server_body(target, None),
            )?,
            None,
        ),
        Command::Pool {
            command: PoolCommand::List,
        } => (client.get("/api/v1/pools", true)?, None),
        Command::Pool {
            command: PoolCommand::Show { pool },
        } => (
            client.get("/api/v1/pools", true)?,
            Some(Selection::Pool(pool)),
        ),
        Command::Pool {
            command: PoolCommand::Ready { pools },
        } => pool_state(client, pools, "ready")?,
        Command::Pool {
            command: PoolCommand::Drain { pools },
        } => pool_state(client, pools, "drain")?,
        Command::Listener {
            command: ListenerCommand::List,
        } => (client.get("/api/v1/listeners", true)?, None),
        Command::Listener {
            command: ListenerCommand::Show { listener },
        } => (
            client.get("/api/v1/listeners", true)?,
            Some(Selection::Listener(listener)),
        ),
        Command::Listener {
            command: ListenerCommand::Ready { listeners },
        } => listener_state(client, listeners, "ready")?,
        Command::Listener {
            command: ListenerCommand::Drain { listeners },
        } => listener_state(client, listeners, "drain")?,
        Command::Listener {
            command: ListenerCommand::Maintenance { listeners },
        } => listener_state(client, listeners, "maintenance")?,
        Command::Generation {
            command: GenerationCommand::Status,
        } => (client.get("/api/v1/generations", true)?, None),
        Command::Generation {
            command: GenerationCommand::Reload,
        } => (
            client.mutation_json("POST", "/api/v1/generations/reload", &json!({}))?,
            None,
        ),
        Command::Generation {
            command: GenerationCommand::Rollback,
        } => (
            client.mutation_json("POST", "/api/v1/generations/rollback", &json!({}))?,
            None,
        ),
        Command::Generation {
            command: GenerationCommand::Drain { timeout_ms },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/generations/drain",
                &json!({ "timeoutMs": timeout_ms }),
            )?,
            None,
        ),
        Command::Config {
            command: ConfigCommand::Get,
        } => (client.get("/api/v1/config", true)?, None),
        Command::Config {
            command: ConfigCommand::Validate { file },
        } => (
            client.json(
                "POST",
                "/api/v1/config/validate",
                &json!({ "config": config_file(file)? }),
                &[],
            )?,
            None,
        ),
        Command::Config {
            command: ConfigCommand::Apply { file },
        } => {
            let current = client.get("/api/v1/config", true)?.json()?;
            let revision = current
                .get("diskRevision")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CliError::Local("configuration response has no disk revision".into())
                })?;
            (
                client.json(
                    "PUT",
                    "/api/v1/config",
                    &json!({ "config": config_file(file)? }),
                    &[("If-Config-Revision", revision)],
                )?,
                None,
            )
        }
        Command::Config {
            command: ConfigCommand::Diff { file },
        } => {
            let current = client.get("/api/v1/config", true)?.json()?;
            let active = current.get("config").cloned().ok_or_else(|| {
                CliError::Local("configuration response has no config field".into())
            })?;
            let candidate = config_file(file)?;
            let value = json!({ "different": active != candidate, "active": active, "candidate": candidate });
            print_value(&value, cli.output, cli.quiet)?;
            return Ok(FollowOutcome::Complete);
        }
        Command::Tls {
            command: TlsCommand::List,
        } => (client.get("/api/v1/tls", true)?, None),
        Command::Tls {
            command: TlsCommand::Reconcile { certificate },
        } => (
            client.mutation_json(
                "POST",
                "/api/v1/tls/reconcile",
                &json!({ "certificate": certificate }),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::List,
                },
        } => (client.get("/api/v1/rtmp/streams", true)?, None),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::Show { stream },
                },
        } => (
            client.get(&format!("/api/v1/rtmp/streams/{stream}"), true)?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Recorder {
                    command: RecorderCommand::Start { stream, recorder },
                },
        } => (
            client.generation_json(
                "POST",
                &format!("/api/v1/rtmp/streams/{stream}/recorders/{recorder}/start"),
                &json!({}),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Recorder {
                    command: RecorderCommand::Stop { stream, recorder },
                },
        } => (
            client.generation_json(
                "POST",
                &format!("/api/v1/rtmp/streams/{stream}/recorders/{recorder}/stop"),
                &json!({}),
            )?,
            None,
        ),
        Command::Rtmp {
            command:
                RtmpCommand::Stream {
                    command: StreamCommand::Disconnect { .. },
                }
                | RtmpCommand::Relay {
                    command: RelayCommand::Reconnect { .. },
                },
        } => {
            return Err(CliError::Unsupported(
                "the active RTMP registry does not own a safe cancellation handle for this operation",
            ));
        }
        Command::Serve { .. }
        | Command::Import { .. }
        | Command::Version
        | Command::Config {
            command: ConfigCommand::Check { .. } | ConfigCommand::Compose { .. },
        } => {
            return Err(CliError::Local(
                "local command was dispatched through the management client".into(),
            ));
        }
    };
    let value = response.output_value()?;
    let value = match selection {
        Some(selection) => selection.apply(value)?,
        None => value,
    };
    print_value(&value, cli.output, cli.quiet)?;
    Ok(FollowOutcome::Complete)
}

fn follow_events(cli: &Cli, client: &Client) -> Result<(), CliError> {
    let Command::Events {
        command:
            EventsCommand::Follow {
                after,
                limit,
                interval_ms,
            },
    } = cli.command()
    else {
        return Ok(());
    };
    let mut cursor = *after;
    loop {
        let value = client
            .get(
                &format!("/api/v1/events?after={cursor}&limit={limit}"),
                true,
            )?
            .json()?;
        if let Some(events) = value.get("events").and_then(Value::as_array) {
            for event in events {
                print_value(event, cli.output, cli.quiet)?;
            }
        }
        cursor = value
            .get("cursor")
            .and_then(Value::as_u64)
            .unwrap_or(cursor);
        thread::sleep(Duration::from_millis(*interval_ms));
    }
}

fn server_state(
    client: &Client,
    target: &ServerTarget,
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/servers/administrative-state",
            &server_body(target, Some(("state", state))),
        )?,
        None,
    ))
}

fn pool_state(
    client: &Client,
    pools: &[String],
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/pools/administrative-state",
            &json!({ "pools": pools, "state": state }),
        )?,
        None,
    ))
}

fn listener_state(
    client: &Client,
    listeners: &[String],
    state: &str,
) -> Result<(Response, Option<Selection<'static>>), CliError> {
    Ok((
        client.mutation_json(
            "POST",
            "/api/v1/listeners/administrative-state",
            &json!({ "listeners": listeners, "state": state }),
        )?,
        None,
    ))
}

fn server_targets(target: &ServerTarget) -> Vec<Value> {
    target
        .pools
        .iter()
        .map(|pool| json!({ "pool": pool, "server": target.server }))
        .collect()
}

fn server_body(target: &ServerTarget, field: Option<(&str, &str)>) -> Value {
    let mut body = json!({ "targets": server_targets(target) });
    if let Some((name, value)) = field {
        body.as_object_mut()
            .expect("object")
            .insert(name.into(), json!(value));
    }
    body
}

fn server_bool_body(target: &ServerTarget, field: &str, value: bool) -> Value {
    let mut body = server_body(target, None);
    body.as_object_mut()
        .expect("object")
        .insert(field.into(), json!(value));
    body
}

fn server_capacity_body(target: &ServerTarget, limit: Option<u64>) -> Value {
    let mut body = server_body(target, None);
    body.as_object_mut()
        .expect("object")
        .insert("maxConnections".into(), json!(limit));
    body
}

impl HealthValue {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

enum Selection<'a> {
    Server(&'a ServerTarget),
    Pool(&'a str),
    Listener(&'a str),
}

impl Selection<'_> {
    fn apply(self, mut value: Value) -> Result<Value, CliError> {
        match self {
            Self::Server(target) => {
                let servers = value
                    .get_mut("servers")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| {
                        CliError::Local("server response has no servers array".into())
                    })?;
                servers.retain(|entry| {
                    target
                        .pools
                        .iter()
                        .any(|pool| entry.get("pool").and_then(Value::as_str) == Some(pool))
                        && entry.pointer("/server/name").and_then(Value::as_str)
                            == Some(&target.server)
                });
                if servers.is_empty() {
                    return Err(CliError::Status(404, "server_not_found".into()));
                }
                if !target.pools.iter().all(|pool| {
                    servers
                        .iter()
                        .any(|entry| entry.get("pool").and_then(Value::as_str) == Some(pool))
                }) {
                    return Err(CliError::Status(404, "server_not_found".into()));
                }
            }
            Self::Pool(name) => filter_named(&mut value, "pools", name)?,
            Self::Listener(name) => filter_named(&mut value, "listeners", name)?,
        }
        Ok(value)
    }
}

fn filter_named(value: &mut Value, field: &str, name: &str) -> Result<(), CliError> {
    let entries = value
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CliError::Local(format!("response has no {field} array")))?;
    entries.retain(|entry| entry.get("name").and_then(Value::as_str) == Some(name));
    if entries.is_empty() {
        return Err(CliError::Status(404, format!("{field}_not_found")));
    }
    Ok(())
}

fn config_file(path: &Path) -> Result<Value, CliError> {
    let bytes = read_regular_file(path, MAX_FILE_BYTES, false)
        .map_err(|_| CliError::Local("configuration file could not be read securely".into()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| CliError::Local("configuration file must contain JSON".into()))?;
    Ok(value.get("config").cloned().unwrap_or(value))
}

fn read_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let bytes = Zeroizing::new(
        read_regular_file(path, MAX_TOKEN_BYTES + 2, true).map_err(|_| {
            CliError::Local("management token file could not be read securely".into())
        })?,
    );
    let mut token = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CliError::Local("management token file is invalid".into()))?,
    );
    while token.ends_with(['\n', '\r']) {
        token.pop();
    }
    if token.len() < 32
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(CliError::Local("management token file is invalid".into()));
    }
    Ok(token)
}

fn read_regular_file(path: &Path, max_bytes: usize, secure_mode: bool) -> io::Result<Vec<u8>> {
    let descriptor = rustix_fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let before = rustix_fs::fstat(&descriptor)?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || (secure_mode && !matches!(before.st_mode & 0o7777, 0o400 | 0o600))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file does not meet the required policy",
        ));
    }
    let size = usize::try_from(before.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is too large"))?;
    if size > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is too large",
        ));
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(size);
    io::Read::by_ref(&mut file)
        .take(u64::try_from(max_bytes + 1).expect("file bound fits u64"))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is too large",
        ));
    }
    let after = rustix_fs::fstat(&file)?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
        || before.st_mode != after.st_mode
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed while it was read",
        ));
    }
    Ok(bytes)
}

fn print_value(value: &Value, output: Output, quiet: bool) -> Result<(), CliError> {
    if quiet {
        return Ok(());
    }
    match output {
        Output::Json => println!(
            "{}",
            serde_json::to_string(value).map_err(|error| CliError::Local(error.to_string()))?
        ),
        Output::Table => print_table(value),
        Output::Plain => print_plain(value),
    }
    Ok(())
}

fn print_table(value: &Value) {
    let Some(object) = value.as_object() else {
        print_plain(value);
        return;
    };
    let Some((_, rows)) = object
        .iter()
        .find_map(|(name, value)| value.as_array().map(|rows| (name, rows)))
    else {
        for (name, value) in object {
            println!("{name}\t{}", table_cell(value));
        }
        return;
    };
    let mut columns = Vec::<&str>::new();
    for row in rows {
        if let Some(row) = row.as_object() {
            for name in row.keys() {
                if !columns.contains(&name.as_str()) {
                    columns.push(name);
                }
            }
        }
    }
    if columns.is_empty() {
        rows.iter().for_each(print_plain);
        return;
    }
    println!("{}", columns.join("\t"));
    for row in rows {
        if let Some(row) = row.as_object() {
            let values = columns
                .iter()
                .map(|column| row.get(*column).map_or_else(String::new, table_cell))
                .collect::<Vec<_>>();
            println!("{}", values.join("\t"));
        }
    }
}

fn table_cell(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
        .replace(['\n', '\r', '\t'], " ")
}

fn print_plain(value: &Value) {
    match value {
        Value::Null => println!("null"),
        Value::Bool(value) => println!("{value}"),
        Value::Number(value) => println!("{value}"),
        Value::String(value) => println!("{value}"),
        Value::Array(values) => values.iter().for_each(print_plain),
        Value::Object(values) => {
            for (name, value) in values {
                if value.is_object() || value.is_array() {
                    println!("{name}={value}");
                } else if let Some(value) = value.as_str() {
                    println!("{name}={value}");
                } else {
                    println!("{name}={value}");
                }
            }
        }
    }
}

struct Endpoint {
    authority: String,
    address: String,
}

impl Endpoint {
    fn parse(value: &str) -> Result<Self, CliError> {
        let authority = value
            .strip_prefix("http://")
            .ok_or_else(|| CliError::Local("endpoint must use http://".into()))?;
        if authority.is_empty() || authority.contains('/') || authority.contains('@') {
            return Err(CliError::Local(
                "endpoint must contain only an HTTP authority".into(),
            ));
        }
        let address = if authority.starts_with('[') {
            let end = authority
                .find(']')
                .ok_or_else(|| CliError::Local("endpoint IPv6 authority is invalid".into()))?;
            let host = &authority[1..end];
            let port = authority
                .get(end + 1..)
                .and_then(|rest| rest.strip_prefix(':'))
                .unwrap_or("80");
            format!("[{host}]:{port}")
        } else if authority.rsplit_once(':').is_some() {
            authority.to_owned()
        } else {
            format!("{authority}:80")
        };
        Ok(Self {
            authority: authority.to_owned(),
            address,
        })
    }
}

struct Client {
    endpoint: Endpoint,
    token: Option<Zeroizing<String>>,
}

impl Client {
    fn get(&self, path: &str, authenticated: bool) -> Result<Response, CliError> {
        self.request("GET", path, &[], &[], authenticated)
    }

    fn json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
        headers: &[(&str, &str)],
    ) -> Result<Response, CliError> {
        let body = serde_json::to_vec(value).map_err(|error| CliError::Local(error.to_string()))?;
        self.request(method, path, &body, headers, true)
    }

    fn mutation_json(&self, method: &str, path: &str, value: &Value) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        let mut value = value.clone();
        value
            .as_object_mut()
            .ok_or_else(|| CliError::Local("mutation body must be an object".into()))?
            .insert("expectedActiveRevision".into(), json!(revision));
        self.json(method, path, &value, &[])
    }

    fn generation_json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
    ) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        self.json(
            method,
            path,
            value,
            &[("If-Generation-Revision", &revision)],
        )
    }

    fn active_revision(&self) -> Result<String, CliError> {
        self.get("/api/v1/generations", true)?
            .json()?
            .pointer("/generation/activeRevision")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CliError::Local("generation response has no active revision".into()))
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
        authenticated: bool,
    ) -> Result<Response, CliError> {
        if !path.starts_with('/') || path.contains(['\r', '\n']) {
            return Err(CliError::Local("request path is invalid".into()));
        }
        let address = self
            .endpoint
            .address
            .to_socket_addrs()
            .map_err(|_| CliError::Connect("endpoint could not be resolved".into()))?
            .next()
            .ok_or_else(|| CliError::Connect("endpoint did not resolve to an address".into()))?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .map_err(|_| CliError::Connect("endpoint connection failed".into()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| CliError::Connect(error.to_string()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| CliError::Connect(error.to_string()))?;
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n", self.endpoint.authority, body.len()).map_err(connect_io)?;
        if !body.is_empty() {
            stream
                .write_all(b"Content-Type: application/json\r\n")
                .map_err(connect_io)?;
        }
        if authenticated {
            let token = self.token.as_deref().ok_or_else(|| {
                CliError::Local("--token-file or OXIROUTE_MANAGEMENT_TOKEN_FILE is required".into())
            })?;
            write!(stream, "Authorization: Bearer {token}\r\n").map_err(connect_io)?;
        }
        for (name, value) in headers {
            if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(CliError::Local("request header is invalid".into()));
            }
            write!(stream, "{name}: {value}\r\n").map_err(connect_io)?;
        }
        stream.write_all(b"\r\n").map_err(connect_io)?;
        stream.write_all(body).map_err(connect_io)?;
        Response::read(BufReader::new(stream))?.into_result()
    }
}

fn connect_io(_: io::Error) -> CliError {
    CliError::Connect("endpoint I/O failed".into())
}

#[derive(Clone, Copy)]
enum BodyFraming {
    None,
    Length(usize),
    Chunked,
    Close,
}

struct ParsedHead {
    status: u16,
    content_type: String,
    framing: BodyFraming,
}

impl ParsedHead {
    fn parse(bytes: &[u8]) -> Result<Self, CliError> {
        let headers = std::str::from_utf8(bytes)
            .map_err(|_| CliError::Remote("invalid HTTP response headers".into()))?;
        let mut lines = headers.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| CliError::Remote("invalid HTTP response status".into()))?;
        let mut status_fields = status_line.split_whitespace();
        if status_fields.next() != Some("HTTP/1.1") {
            return Err(CliError::Remote("endpoint did not return HTTP/1.1".into()));
        }
        let status: u16 = status_fields
            .next()
            .and_then(|value| value.parse().ok())
            .filter(|value| (100..=599).contains(value))
            .ok_or_else(|| CliError::Remote("invalid HTTP response status".into()))?;
        let mut content_type = String::new();
        let mut content_lengths = Vec::new();
        let mut transfer_codings = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| CliError::Remote("invalid HTTP response header".into()))?;
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-type") {
                value.clone_into(&mut content_type);
            } else if name.eq_ignore_ascii_case("content-length") {
                for value in value.split(',') {
                    let length = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| CliError::Remote("invalid HTTP Content-Length".into()))?;
                    content_lengths.push(length);
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                transfer_codings.extend(
                    value
                        .split(',')
                        .map(|coding| coding.trim().to_ascii_lowercase()),
                );
            }
        }
        if content_lengths
            .windows(2)
            .any(|values| values[0] != values[1])
        {
            return Err(CliError::Remote(
                "conflicting HTTP Content-Length values".into(),
            ));
        }
        if !transfer_codings.is_empty() && !content_lengths.is_empty() {
            return Err(CliError::Remote(
                "HTTP response contains both Transfer-Encoding and Content-Length".into(),
            ));
        }
        let framing = if (100..200).contains(&status) || matches!(status, 204 | 304) {
            BodyFraming::None
        } else if !transfer_codings.is_empty() {
            if transfer_codings != ["chunked"] {
                return Err(CliError::Remote(
                    "unsupported HTTP Transfer-Encoding".into(),
                ));
            }
            BodyFraming::Chunked
        } else if let Some(length) = content_lengths.first().copied() {
            if length > MAX_RESPONSE_BYTES {
                return Err(CliError::Remote(
                    "response exceeded the client limit".into(),
                ));
            }
            BodyFraming::Length(length)
        } else {
            BodyFraming::Close
        };
        Ok(Self {
            status,
            content_type,
            framing,
        })
    }
}

fn read_http_head(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader.read_until(b'\n', &mut line).map_err(connect_io)?;
        if read == 0 || !line.ends_with(b"\r\n") {
            return Err(CliError::Remote("truncated HTTP response headers".into()));
        }
        if head.len().saturating_add(line.len()) > MAX_HEADER_BYTES {
            return Err(CliError::Remote(
                "HTTP response headers exceeded the client limit".into(),
            ));
        }
        if line == b"\r\n" {
            return Ok(head);
        }
        head.extend_from_slice(&line);
    }
}

fn read_http_body(reader: &mut impl BufRead, framing: BodyFraming) -> Result<Vec<u8>, CliError> {
    match framing {
        BodyFraming::None => Ok(Vec::new()),
        BodyFraming::Length(length) => {
            let mut body = vec![0; length];
            reader
                .read_exact(&mut body)
                .map_err(|_| CliError::Remote("truncated HTTP response".into()))?;
            Ok(body)
        }
        BodyFraming::Chunked => read_chunked_body(reader),
        BodyFraming::Close => {
            let mut body = Vec::new();
            reader
                .take(u64::try_from(MAX_RESPONSE_BYTES + 1).expect("response bound fits u64"))
                .read_to_end(&mut body)
                .map_err(connect_io)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(CliError::Remote(
                    "response exceeded the client limit".into(),
                ));
            }
            Ok(body)
        }
    }
}

fn read_chunked_body(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut body = Vec::new();
    loop {
        let line = read_http_line(reader)?;
        let size = line
            .split(|byte| *byte == b';')
            .next()
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| usize::from_str_radix(value.trim(), 16).ok())
            .ok_or_else(|| CliError::Remote("invalid HTTP chunk size".into()))?;
        if size == 0 {
            let mut trailer_bytes = 0_usize;
            loop {
                let trailer = read_http_line(reader)?;
                trailer_bytes = trailer_bytes.saturating_add(trailer.len() + 2);
                if trailer_bytes > MAX_HEADER_BYTES {
                    return Err(CliError::Remote(
                        "HTTP trailers exceeded the client limit".into(),
                    ));
                }
                if trailer.is_empty() {
                    return Ok(body);
                }
                if !trailer.contains(&b':') {
                    return Err(CliError::Remote("invalid HTTP trailer".into()));
                }
            }
        }
        if size > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(CliError::Remote(
                "response exceeded the client limit".into(),
            ));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(&mut body[start..])
            .map_err(|_| CliError::Remote("truncated HTTP chunk".into()))?;
        let mut terminator = [0; 2];
        reader
            .read_exact(&mut terminator)
            .map_err(|_| CliError::Remote("truncated HTTP chunk".into()))?;
        if terminator != *b"\r\n" {
            return Err(CliError::Remote("invalid HTTP chunk terminator".into()));
        }
    }
}

fn read_http_line(reader: &mut impl BufRead) -> Result<Vec<u8>, CliError> {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).map_err(connect_io)?;
    if !line.ends_with(b"\r\n") || line.len() > MAX_HEADER_BYTES {
        return Err(CliError::Remote("invalid HTTP framing line".into()));
    }
    line.truncate(line.len() - 2);
    Ok(line)
}

#[derive(Debug)]
struct Response {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Response {
    #[cfg(test)]
    fn parse(bytes: &[u8]) -> Result<Self, CliError> {
        Self::read(BufReader::new(Cursor::new(bytes)))
    }

    fn read(mut reader: impl BufRead) -> Result<Self, CliError> {
        for _ in 0..=8 {
            let head = read_http_head(&mut reader)?;
            let parsed = ParsedHead::parse(&head)?;
            if (100..200).contains(&parsed.status) && parsed.status != 101 {
                continue;
            }
            let body = read_http_body(&mut reader, parsed.framing)?;
            return Ok(Self {
                status: parsed.status,
                content_type: parsed.content_type,
                body,
            });
        }
        Err(CliError::Remote(
            "endpoint returned too many interim responses".into(),
        ))
    }

    fn into_result(self) -> Result<Self, CliError> {
        if (200..300).contains(&self.status) {
            Ok(self)
        } else {
            let code = serde_json::from_slice::<Value>(&self.body)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/code")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| format!("http_{}", self.status));
            Err(CliError::Status(self.status, code))
        }
    }

    fn json(&self) -> Result<Value, CliError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| CliError::Remote("endpoint returned invalid JSON".into()))
    }

    fn output_value(&self) -> Result<Value, CliError> {
        if self.content_type.starts_with("application/json") {
            self.json()
        } else {
            let text = std::str::from_utf8(&self.body)
                .map_err(|_| CliError::Remote("endpoint returned invalid UTF-8".into()))?;
            Ok(json!({ "contentType": self.content_type, "body": text }))
        }
    }
}

#[derive(Debug)]
enum CliError {
    Local(String),
    Connect(String),
    Status(u16, String),
    Remote(String),
    Unsupported(&'static str),
}

impl CliError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Local(_) => EXIT_LOCAL,
            Self::Connect(_) => EXIT_CONNECT,
            Self::Status(401 | 403, _) => EXIT_AUTH,
            Self::Status(404, _) => EXIT_NOT_FOUND,
            Self::Status(409 | 412, _) => EXIT_CONFLICT,
            Self::Status(_, _) | Self::Remote(_) => EXIT_REMOTE,
            Self::Unsupported(_) => EXIT_UNSUPPORTED,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(message) | Self::Connect(message) | Self::Remote(message) => {
                formatter.write_str(message)
            }
            Self::Status(status, code) => write!(
                formatter,
                "management endpoint returned HTTP {status} ({code})"
            ),
            Self::Unsupported(message) => write!(formatter, "unsupported: {message}"),
        }
    }
}

impl Error for CliError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_pool_targets_and_stable_global_options() {
        let cli = Cli::try_parse_from([
            "oxiroute",
            "--endpoint",
            "http://127.0.0.1:9900",
            "--output",
            "json",
            "server",
            "drain",
            "--pool",
            "public-v4",
            "--pool",
            "public-v6",
            "origin-a",
        ])
        .expect("CLI");
        let Command::Server {
            command: ServerCommand::Drain(target),
        } = cli.command()
        else {
            panic!("server drain command")
        };
        assert_eq!(target.pools, ["public-v4", "public-v6"]);
        assert_eq!(target.server, "origin-a");
        assert!(matches!(cli.output, Output::Json));
    }

    #[test]
    fn combines_runtime_offline_and_management_commands() {
        let explicit =
            Cli::try_parse_process_from(["oxiroute", "serve", "edge.lua"]).expect("explicit serve");
        let legacy = Cli::try_parse_process_from(["oxiroute", "edge.lua"]).expect("legacy serve");
        let check = Cli::try_parse_process_from(["oxiroute", "config", "check", "edge.lua"])
            .expect("config check");
        let get = Cli::try_parse_process_from(["oxiroute", "config", "get"]).expect("config get");
        let default = Cli::try_parse_process_from(["oxiroute"]).expect("default serve");

        assert!(matches!(
            explicit.command(),
            Command::Serve { config } if config == &PathBuf::from("edge.lua")
        ));
        assert!(matches!(
            legacy.command(),
            Command::Serve { config } if config == &PathBuf::from("edge.lua")
        ));
        assert!(matches!(
            check.command(),
            Command::Config {
                command: ConfigCommand::Check { config }
            } if config == &PathBuf::from("edge.lua")
        ));
        assert!(matches!(
            get.command(),
            Command::Config {
                command: ConfigCommand::Get
            }
        ));
        assert!(matches!(
            default.command(),
            Command::Serve { config } if config == &PathBuf::from("oxiroute.kdl")
        ));
    }

    #[test]
    fn parses_canonical_config_composition_inputs_in_order() {
        let cli = Cli::try_parse_process_from([
            "oxiroute",
            "config",
            "compose",
            "--format",
            "hocon",
            "nginx.lua",
            "haproxy.lua",
        ])
        .expect("config compose");

        assert!(matches!(
            cli.command(),
            Command::Config {
                command: ConfigCommand::Compose { format, configs }
            } if *format == ComposeFormat::Hocon
                && configs == &[PathBuf::from("nginx.lua"), PathBuf::from("haproxy.lua")]
        ));
    }

    #[test]
    fn parses_explicit_haproxy_preprocessing_environment() {
        let cli = Cli::try_parse_process_from([
            "oxiroute",
            "import",
            "haproxy",
            "/etc/haproxy/haproxy.cfg",
            "--node-ip",
            "10.0.0.15",
            "--gpu1-defined",
            "--shadow-port-offset",
            "10000",
            "--format",
            "hocon",
            "--output",
            "preview",
        ])
        .expect("HAProxy import");

        let Command::Import {
            command:
                ImportCommand::Haproxy {
                    configs,
                    node_ip,
                    gpu1_defined,
                    shadow_port_offset,
                    format,
                    output,
                },
        } = cli.command()
        else {
            panic!("HAProxy import command")
        };
        assert_eq!(configs, &[PathBuf::from("/etc/haproxy/haproxy.cfg")]);
        assert_eq!(*node_ip, Some("10.0.0.15".parse().unwrap()));
        assert!(*gpu1_defined);
        assert_eq!(*shadow_port_offset, Some(10_000));
        assert_eq!(*format, ComposeFormat::Hocon);
        assert!(matches!(output, ImportOutput::Preview));
    }

    #[test]
    fn parses_nginx_timezone_and_shadow_inputs() {
        let cli = Cli::try_parse_process_from([
            "oxiroute",
            "import",
            "nginx",
            "/etc/nginx/nginx.conf",
            "--host-timezone",
            "America/Bahia",
            "--default-access-log-file",
            "/var/lib/oxiroute/http-access.jsonl",
            "--recording-root",
            "/mnt/cloud/4tb/cam-rtmp",
            "--default-error-server",
            "nginx/1.30.2",
            "--shadow-port-offset",
            "10000",
            "--format",
            "lua",
            "--output",
            "preview",
        ])
        .expect("nginx import");

        assert!(matches!(
            cli.command(),
            Command::Import {
                command: ImportCommand::Nginx {
                    host_timezone: Some(timezone),
                    default_access_log_file: Some(path),
                    recording_root: Some(recording_root),
                    default_error_server: Some(server),
                    shadow_port_offset: Some(10_000),
                    format: ComposeFormat::Lua,
                    output: ImportOutput::Preview,
                    ..
                }
            } if timezone == "America/Bahia"
                && path == Path::new("/var/lib/oxiroute/http-access.jsonl")
                && recording_root == Path::new("/mnt/cloud/4tb/cam-rtmp")
                && server == "nginx/1.30.2"
        ));
    }

    #[test]
    fn haproxy_gpu_presence_requires_a_node_ip() {
        assert!(
            Cli::try_parse_process_from([
                "oxiroute",
                "import",
                "haproxy",
                "/etc/haproxy/haproxy.cfg",
                "--gpu1-defined",
            ])
            .is_err()
        );
    }

    #[test]
    fn haproxy_shadow_preview_shifts_only_listener_ports() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../oxiroute-import/tests/fixtures/live/phoenix/haproxy.cfg");
        let output = import_haproxy(
            &[fixture],
            Some("10.0.0.15".parse().unwrap()),
            true,
            Some(10_000),
            ConfigFormat::Hocon,
            ImportOutput::Preview,
        )
        .expect("shadow preview");

        for listener in ["10.0.0.15:20440", "10.0.0.15:22002", "10.0.0.15:18080"] {
            assert!(output.contains(listener), "missing {listener}");
        }
        assert!(output.contains("127.0.0.1:10450"));
        assert!(output.contains("127.0.0.1:10451"));
        assert!(
            oxiroute_config_source::decode_value(ConfigFormat::Hocon, output.as_bytes()).is_ok()
        );
    }

    #[test]
    fn haproxy_shadow_preview_rejects_port_overflow_and_report_mode() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../oxiroute-import/tests/fixtures/live/phoenix/haproxy.cfg");
        let paths = [fixture];
        let node_ip = Some("10.0.0.15".parse().unwrap());

        assert!(
            import_haproxy(
                &paths,
                node_ip,
                true,
                Some(60_000),
                ConfigFormat::Kdl,
                ImportOutput::Preview,
            )
            .is_err()
        );
        assert!(
            import_haproxy(
                &paths,
                node_ip,
                true,
                Some(10_000),
                ConfigFormat::Kdl,
                ImportOutput::Report,
            )
            .is_err()
        );
    }

    #[test]
    fn response_parser_preserves_json_fields_and_maps_exit_categories() {
        let response = Response::parse(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"ready\":true,\"value\":7}",
        )
        .expect("response");
        assert_eq!(
            response.json().expect("JSON"),
            json!({ "ready": true, "value": 7 })
        );
        assert_eq!(
            CliError::Status(401, "unauthorized".into()).exit_code(),
            EXIT_AUTH
        );
        assert_eq!(
            CliError::Status(404, "missing".into()).exit_code(),
            EXIT_NOT_FOUND
        );
        assert_eq!(
            CliError::Status(409, "conflict".into()).exit_code(),
            EXIT_CONFLICT
        );
    }

    #[test]
    fn token_errors_never_include_token_contents() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().expect("directory");
        let path = directory.path().join("token");
        std::fs::write(&path, "secret").expect("token");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        let error = read_token(&path).expect_err("short token");
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn response_parser_supports_interim_and_chunked_framing() {
        let response = Response::parse(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"ok\":1\r\n1\r\n}\r\n0\r\nX-Test: yes\r\n\r\n",
        )
        .expect("chunked response");
        assert_eq!(response.json().expect("JSON"), json!({ "ok": 1 }));
    }

    #[test]
    fn response_parser_rejects_conflicting_lengths() {
        let error =
            Response::parse(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx")
                .expect_err("conflicting lengths");
        assert!(error.to_string().contains("conflicting"));
    }

    #[test]
    fn rejects_empty_batch_targets() {
        assert!(Cli::try_parse_from(["oxiroute", "pool", "drain"]).is_err());
        assert!(Cli::try_parse_from(["oxiroute", "listener", "ready", ""]).is_err());
        assert!(
            Cli::try_parse_from(["oxiroute", "server", "show", "--pool", "", "origin"]).is_err()
        );
    }
}
