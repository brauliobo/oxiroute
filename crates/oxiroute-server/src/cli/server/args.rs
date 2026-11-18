use std::{net::SocketAddr, path::PathBuf, time::Duration};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::cli::{
    change::{Desired, Persist, Plan, Scope, Setting},
    parse::{nonempty_target, parse_duration, parse_label},
};

#[derive(Subcommand)]
pub(crate) enum ServerCommand {
    /// List servers; `--pool` also works against a registry read socket or a read token.
    List {
        /// Only the servers of this pool.
        #[arg(long = "pool", value_parser = nonempty_target)]
        pool: Option<String>,
        /// Only servers from the configuration (`static`) or from registrations (`dynamic`).
        #[arg(long, value_enum)]
        origin: Option<OriginFilter>,
    },
    /// Register a server in a pool that accepts registrations, or keep it registered.
    Register(RegisterArgs),
    /// Remove a registered server.
    Deregister {
        /// Pool the server is registered in.
        #[arg(long = "pool", required = true, value_parser = nonempty_target)]
        pool: String,
        /// Name of the registered server.
        #[arg(value_name = "SERVER", value_parser = nonempty_target)]
        server: String,
        /// Succeed when the server is not registered.
        #[arg(long)]
        missing_ok: bool,
    },
    /// Show one server of each named pool.
    Show(ServerTarget),
    /// Return a server to service; the opposite of `drain`.
    #[command(visible_alias = "undrain")]
    Ready(ChangeTarget),
    /// Stop sending new requests to a server; running ones finish and health checks continue.
    Drain(ChangeTarget),
    /// Drain a server, then move the HTTP requests running on it to other servers of its pool.
    ///
    /// Requests that sent no response and can be replayed are cancelled and served by another server
    /// without the caller noticing; the rest keep running as in a drain and are counted in the report.
    Evacuate {
        #[command(flatten)]
        target: ChangeTarget,
        /// Also cancel the requests that cannot move; their callers get an error.
        #[arg(long)]
        force: bool,
    },
    /// Take a server out of service and suspend its health checks.
    Maintenance(ChangeTarget),
    /// Force a server up or down regardless of its health checks, or back to `auto`.
    SetHealth {
        #[command(flatten)]
        target: ChangeTarget,
        /// `up` or `down` forces the health, `auto` follows the health checks.
        #[arg(value_enum)]
        health: HealthValue,
    },
    /// Switch the active health checks of a server on or off.
    Check {
        #[command(flatten)]
        target: ChangeTarget,
        /// Whether the checks run.
        #[arg(value_enum)]
        action: Toggle,
    },
    /// Override or reset the connection limit of a server.
    MaxConnections {
        #[command(subcommand)]
        command: MaxConnectionsCommand,
    },
    /// Override or reset the weight of a server.
    Weight {
        #[command(subcommand)]
        command: WeightCommand,
    },
    /// Resolve the address of a server again now instead of waiting for its DNS refresh.
    RefreshDns(ServerTarget),
}

#[derive(Args)]
pub(crate) struct ServerTarget {
    /// Pool holding the server; repeat to apply to the same server name in several pools.
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pub(super) pools: Vec<String>,
    /// Name of the server.
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

#[derive(Args)]
pub(crate) struct ChangeTarget {
    #[command(flatten)]
    target: ServerTarget,
    #[command(flatten)]
    persist: Persist,
}

impl std::ops::Deref for ChangeTarget {
    type Target = ServerTarget;

    fn deref(&self) -> &ServerTarget {
        &self.target
    }
}

impl ChangeTarget {
    pub(super) fn body(&self, fields: Value) -> Value {
        self.persist.body(self.target.body(fields))
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
    /// Pool to register the server in; the pool must accept registrations.
    #[arg(long = "pool", required = true, value_parser = nonempty_target)]
    pub(super) pool: String,
    /// Name to register the server under.
    #[arg(value_name = "SERVER", value_parser = nonempty_target)]
    pub(super) server: String,
    /// Socket address the proxy connects to, for example 192.0.2.10:8080.
    #[arg(long)]
    pub(super) address: SocketAddr,
    /// Relative weight, 1 to 100; the pool's default when omitted.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub(super) weight: Option<u16>,
    /// Priority group of the server; the pool's default when omitted.
    #[arg(long)]
    pub(super) priority: Option<u16>,
    /// Largest number of simultaneous connections; unlimited when omitted.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub(super) max_connections: Option<u64>,
    /// Lease length such as `500ms`, `15s` or `2m`; the pool's default when omitted.
    #[arg(long, value_parser = parse_duration)]
    pub(super) ttl: Option<Duration>,
    /// Label stored with the registration as KEY=VALUE; repeat for several.
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
    /// Limit the connections a server accepts, overriding the configuration.
    Set {
        #[command(flatten)]
        target: ChangeTarget,
        /// Largest number of simultaneous connections.
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        limit: u64,
    },
    /// Return to the configured connection limit.
    Reset(ChangeTarget),
}

#[derive(Subcommand)]
pub(crate) enum WeightCommand {
    /// Set the share of requests a server receives relative to its pool, overriding the configuration.
    Set {
        #[command(flatten)]
        target: ChangeTarget,
        /// Relative weight, 1 to 100.
        #[arg(value_parser = clap::value_parser!(u16).range(1..=100))]
        weight: u16,
    },
    /// Return to the configured weight.
    Reset(ChangeTarget),
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
