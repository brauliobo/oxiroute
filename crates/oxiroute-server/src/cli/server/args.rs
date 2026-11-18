use std::{net::SocketAddr, path::PathBuf, time::Duration};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::cli::{
    change::{Desired, Plan, Scope, Setting},
    parse::{nonempty_target, parse_label, parse_ttl},
};

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
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
    /// Drain a server, then move the HTTP requests running on it to other servers of its pool.
    ///
    /// Requests that sent no response and can be replayed are cancelled and served by another server
    /// without the caller noticing; the rest keep running as in a drain and are counted in the report.
    Evacuate {
        #[command(flatten)]
        target: ServerTarget,
        /// Also cancel the requests that cannot move; their callers get an error.
        #[arg(long)]
        force: bool,
    },
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
pub(crate) struct ServerTarget {
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pub(super) pools: Vec<String>,
    #[arg(value_name = "SERVER", value_parser = nonempty_target)]
    pub(super) server: String,
}

impl ServerTarget {
    pub(super) fn plan(&self, setting: Setting, desired: Desired) -> Plan {
        Plan::new(Scope::Server, setting, desired, self.names())
    }

    fn names(&self) -> Vec<String> {
        self.pools
            .iter()
            .map(|pool| format!("{pool}/{}", self.server))
            .collect()
    }

    pub(super) fn body(&self, mut fields: Value) -> Value {
        fields["targets"] = self
            .pools
            .iter()
            .map(|pool| json!({ "pool": pool, "server": self.server }))
            .collect();
        fields
    }
}

#[derive(Clone, Copy, Eq, PartialEq, ValueEnum)]
pub(crate) enum OriginFilter {
    Static,
    Dynamic,
}

impl OriginFilter {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
        }
    }
}

#[derive(Args)]
pub(crate) struct RegisterArgs {
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pub(super) pool: String,
    #[arg(value_name = "SERVER", value_parser = nonempty_target)]
    pub(super) server: String,
    #[arg(long)]
    pub(super) address: SocketAddr,
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub(super) weight: Option<u16>,
    #[arg(long)]
    pub(super) priority: Option<u16>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub(super) max_connections: Option<u64>,
    /// Lease length such as `500ms`, `15s` or `2m`; the pool's default when omitted.
    #[arg(long, value_parser = parse_ttl)]
    pub(super) ttl: Option<Duration>,
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    pub(super) labels: Vec<(String, String)>,
    /// JSON file with free-form metadata for read-only consumers.
    #[arg(long, value_name = "FILE")]
    pub(super) metadata_file: Option<PathBuf>,
    /// Stay in the foreground, renew the lease, and deregister on SIGTERM or SIGINT.
    #[arg(long)]
    pub(super) heartbeat: bool,
}

#[derive(Subcommand)]
pub(crate) enum MaxConnectionsCommand {
    Set {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        limit: u64,
    },
    Reset(ServerTarget),
}

#[derive(Subcommand)]
pub(crate) enum WeightCommand {
    Set {
        #[command(flatten)]
        target: ServerTarget,
        #[arg(value_parser = clap::value_parser!(u16).range(1..=100))]
        weight: u16,
    },
    Reset(ServerTarget),
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum HealthValue {
    Auto,
    Up,
    Down,
}

impl HealthValue {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Toggle {
    Enable,
    Disable,
}

impl Toggle {
    pub(super) const fn as_state(self) -> &'static str {
        match self {
            Self::Enable => "enabled",
            Self::Disable => "disabled",
        }
    }
}
