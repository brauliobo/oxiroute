use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::File,
    io::{self, BufRead, BufReader, Read as _, Write as _},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::Duration,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use oxiroute_config_source::{ConfigFormat, render_config};
use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{
    GenerationManager,
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, RejectionDiagnostic},
};

#[cfg(test)]
use std::io::Cursor;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9900";
const DEFAULT_PACKAGE_ENV_FILE: &str = "/etc/oxiroute/oxiroute.env";
const DEFAULT_MANAGEMENT_TOKEN_FILE: &str = "/etc/oxiroute/management.token";
const MANAGEMENT_TOKEN_ENV: &str = "OXIROUTE_MANAGEMENT_TOKEN_FILE";
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PACKAGE_ENV_BYTES: usize = 8 * 1024;
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
    /// Talk to a registry unix socket instead of the management API; no token is used.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[arg(long)]
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
    /// Inspect the requests currently inside the router.
    Requests {
        #[command(subcommand)]
        command: RequestsCommand,
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
    /// Manage the runtime server registry.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
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
        /// Assert that audited upstreams do not emit nginx X-Accel response controls.
        #[arg(long)]
        x_accel_controls_absent: bool,
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
    Squid {
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        /// Shift imported IP socket listener ports for side-by-side validation.
        #[arg(long, value_name = "PORTS", value_parser = clap::value_parser!(u16).range(1..))]
        shadow_port_offset: Option<u16>,
        /// Render preview output in this canonical configuration format.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        #[arg(long, value_enum, default_value_t = ImportOutput::Report)]
        output: ImportOutput,
    },
    Apache {
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        /// Shift imported IP socket listeners for side-by-side validation.
        #[arg(long, value_name = "PORTS", value_parser = clap::value_parser!(u16).range(1..))]
        shadow_port_offset: Option<u16>,
        /// Render preview output in this canonical configuration format.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        #[arg(long, value_enum, default_value_t = ImportOutput::Report)]
        output: ImportOutput,
    },
    Varnish {
        #[arg(value_name = "CONFIG")]
        config: PathBuf,
        /// Add one exact varnishd argument used to interpret the VCL source.
        #[arg(
            long = "arg",
            value_name = "ARG",
            action = clap::ArgAction::Append,
            allow_hyphen_values = true
        )]
        arguments: Vec<String>,
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
pub enum RequestsCommand {
    /// List live requests, oldest first.
    List {
        #[arg(long)]
        pool: Option<String>,
        #[arg(long)]
        route: Option<String>,
        /// Only requests older than this, for example 500ms, 30s, 2m or 1h.
        #[arg(long, value_parser = parse_age_ms)]
        older_than: Option<u64>,
        /// One of `routed`, `queued`, `connecting`, `awaiting_headers`, `streaming`, `retry_wait`.
        #[arg(long)]
        phase: Option<String>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    /// Show one live request by its request id.
    Show {
        #[arg(value_name = "REQUEST_ID")]
        id: String,
    },
}

fn parse_age_ms(value: &str) -> Result<u64, String> {
    let (digits, unit_ms) = [("ms", 1), ("s", 1_000), ("m", 60_000), ("h", 3_600_000)]
        .into_iter()
        .find_map(|(suffix, unit)| value.strip_suffix(suffix).map(|digits| (digits, unit)))
        .ok_or("expected a number with a unit: ms, s, m or h")?;
    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(unit_ms))
        .ok_or_else(|| "age is not a valid number".to_owned())
}

fn requests_path(command: &RequestsCommand) -> String {
    match command {
        RequestsCommand::Show { id } => format!("/api/v1/requests?requestId={id}"),
        RequestsCommand::List {
            pool,
            route,
            older_than,
            phase,
            limit,
        } => {
            let filters = [
                pool.as_ref().map(|pool| format!("pool={pool}")),
                route.as_ref().map(|route| format!("route={route}")),
                phase.as_ref().map(|phase| format!("phase={phase}")),
                older_than.map(|age| format!("min_age_ms={age}")),
                Some(format!("limit={limit}")),
            ];
            format!(
                "/api/v1/requests?{}",
                filters.into_iter().flatten().collect::<Vec<_>>().join("&")
            )
        }
    }
}

#[derive(Subcommand)]
pub enum ServerCommand {
    /// List servers; `--pool` also works against a registry read socket or a read token.
    List {
        #[arg(long = "pool", value_parser = nonempty_target)]
        pool: Option<String>,
        #[arg(long, value_enum)]
        origin: Option<OriginFilter>,
    },
    /// Register a server in a pool that accepts registrations, or keep it registered.
    Register(RegisterArgs),
    /// Remove a registered server.
    Deregister {
        #[arg(long = "pool", required = true, value_parser = nonempty_target)]
        pool: String,
        #[arg(value_name = "SERVER", value_parser = nonempty_target)]
        server: String,
        /// Succeed when the server is not registered.
        #[arg(long)]
        missing_ok: bool,
    },
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
    Weight {
        #[command(subcommand)]
        command: WeightCommand,
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

#[derive(Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum OriginFilter {
    Static,
    Dynamic,
}

#[derive(Args)]
pub struct RegisterArgs {
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pool: String,
    #[arg(value_name = "SERVER", value_parser = nonempty_target)]
    server: String,
    #[arg(long)]
    address: SocketAddr,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    weight: Option<u16>,
    #[arg(long)]
    priority: Option<u16>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    max_connections: Option<u64>,
    /// Lease length such as `500ms`, `15s` or `2m`; the pool's default when omitted.
    #[arg(long, value_parser = registry::parse_ttl)]
    ttl: Option<Duration>,
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = registry::parse_label)]
    labels: Vec<(String, String)>,
    /// JSON file with free-form metadata for read-only consumers.
    #[arg(long, value_name = "FILE")]
    metadata_file: Option<PathBuf>,
    /// Stay in the foreground, renew the lease, and deregister on SIGTERM or SIGINT.
    #[arg(long)]
    heartbeat: bool,
}

#[derive(Subcommand)]
pub enum RegistryCommand {
    Token {
        #[command(subcommand)]
        command: RegistryTokenCommand,
    },
}

#[derive(Subcommand)]
pub enum RegistryTokenCommand {
    /// Append a scoped token to a tokens file and print its secret once.
    Create {
        #[arg(long, value_name = "FILE")]
        tokens_file: PathBuf,
        #[arg(long, value_parser = nonempty_target)]
        id: String,
        /// Pool the token may register servers in; repeat for several.
        #[arg(long = "pool", required_unless_present = "read", value_parser = nonempty_target)]
        pools: Vec<String>,
        #[arg(long, requires = "pools")]
        name_prefix: Option<String>,
        /// Create a read-only token instead.
        #[arg(long, conflicts_with = "pools")]
        read: bool,
    },
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

#[derive(Subcommand)]
pub enum WeightCommand {
    Set {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_parser = clap::value_parser!(u16).range(1..=100))]
        weight: u16,
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

mod client;
mod commands;
mod offline;
mod output;
mod registry;
mod token;

#[allow(clippy::wildcard_imports)]
use client::*;
#[allow(clippy::wildcard_imports)]
use commands::*;
pub use offline::execute_offline;
#[cfg(test)]
use offline::import_haproxy;
#[allow(clippy::wildcard_imports)]
use output::*;
#[allow(clippy::wildcard_imports)]
use token::*;

#[cfg(test)]
mod tests;
