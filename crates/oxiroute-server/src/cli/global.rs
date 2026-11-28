use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Args, ValueEnum};
use zeroize::Zeroizing;

use crate::cli::{
    contexts::Selected,
    error::CliError,
    net::{
        CONTEXT_ENV, DEFAULT_ENDPOINT, ENDPOINT_ENV, Endpoint, MANAGEMENT_TOKEN_ENV, Package,
        Timeouts, read_token,
    },
    parse::parse_timeout,
};

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Output {
    Json,
    Table,
    Plain,
}

#[derive(Args)]
pub(crate) struct Global {
    /// Management API URL; the packaged setup and the local config are tried when it is not given.
    #[arg(
        long,
        global = true,
        env = ENDPOINT_ENV,
        hide_env_values = true,
        value_name = "URL",
        help_heading = "Connection",
        long_help = "Management API URL, `http://HOST:PORT`.\n\nWithout it the CLI reads OXIROUTE_ENDPOINT in /etc/oxiroute/oxiroute.env, \
            then the management bind of /etc/oxiroute/oxiroute.{oxi,kdl,kdl2,yaml,yml,toml,lua,conf,hocon,uci}, then uses http://127.0.0.1:9900."
    )]
    pub(super) endpoint: Option<String>,
    /// Named context whose address, token source and timeout apply; flags and variables take precedence.
    #[arg(
        long,
        global = true,
        env = CONTEXT_ENV,
        value_name = "NAME",
        help_heading = "Connection",
        long_help = "Named context from the contexts file ($XDG_CONFIG_HOME/oxiroute/contexts, mode 0600), managed with \
            `oxiroute context`.\n\nIts address, token source and timeout apply where --endpoint, OXIROUTE_ENDPOINT, \
            --token-file, OXIROUTE_MANAGEMENT_TOKEN_FILE and --timeout do not, and before the packaged setup is \
            discovered. Without it the current context of the file applies. A context token is never sent to an endpoint \
            given by flag or variable."
    )]
    pub(super) context: Option<String>,
    /// Talk to a registry unix socket instead of the management API; no token is used.
    #[arg(long, global = true, value_name = "PATH", help_heading = "Connection")]
    pub(super) socket: Option<PathBuf>,
    /// File holding the management bearer token (regular file, mode 0400 or 0600, 32 to 512 characters).
    #[arg(
        long,
        global = true,
        env = MANAGEMENT_TOKEN_ENV,
        hide_env_values = true,
        value_name = "FILE",
        help_heading = "Connection",
        long_help = "File holding the management bearer token: a regular file with mode 0400 or 0600 and 32 to 512 visible characters.\n\n\
            Without it the CLI reads OXIROUTE_MANAGEMENT_TOKEN_FILE in /etc/oxiroute/oxiroute.env, then the management token_file of \
            /etc/oxiroute/oxiroute.{oxi,kdl,kdl2,yaml,yml,toml,lua,conf,hocon,uci}, then /etc/oxiroute/management.token when it exists."
    )]
    pub(super) token_file: Option<PathBuf>,
    /// Time limit for connecting and for each answer; `--wait` waits up to the same limit.
    #[arg(
        long,
        global = true,
        value_parser = parse_timeout,
        value_name = "DURATION",
        help_heading = "Connection",
        long_help = "Time limit for connecting to the daemon and for each answer, such as 500ms, 15s or 2m.\n\n\
            Without it the CLI waits 5s to connect and 10s for each answer. With --wait on `ready`, `reload` or `generation reload` the \
            same limit bounds the whole wait, 30s by default."
    )]
    pub(super) timeout: Option<Duration>,
    /// Output format of results.
    #[arg(long, global = true, value_enum, default_value_t = Output::Table, value_name = "FORMAT")]
    pub(super) output: Output,
    /// Suppress normal output; errors still go to stderr and the exit code is unchanged.
    #[arg(long, global = true)]
    pub(super) quiet: bool,
}

impl Global {
    pub(super) fn requested_context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(super) fn timeouts(&self, selected: Option<&Selected>) -> Timeouts {
        self.timeout
            .or_else(|| selected.and_then(Selected::timeout))
            .map_or(Timeouts::DEFAULT, Timeouts::limited)
    }

    pub(super) fn endpoint(
        &self,
        package: &Package,
        selected: Option<&Selected>,
    ) -> Result<Endpoint, CliError> {
        if let Some(path) = &self.socket {
            return Endpoint::unix(&path.to_string_lossy());
        }
        let endpoint = self
            .endpoint
            .clone()
            .or_else(|| selected.map(|selected| selected.entry.endpoint.clone()))
            .or_else(|| package.discovered_endpoint())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        Endpoint::parse(&endpoint)
    }

    pub(super) fn token_file<'a>(&'a self, selected: Option<&'a Selected>) -> Option<&'a Path> {
        self.token_file.as_deref().or_else(|| {
            selected
                .and_then(Selected::token_file)
                .filter(|_| self.endpoint.is_none())
        })
    }

    pub(super) fn token(
        &self,
        package: &Package,
        selected: Option<&Selected>,
    ) -> Result<Option<Zeroizing<String>>, CliError> {
        let source = selected
            .and_then(|selected| selected.entry.token.as_ref())
            .filter(|_| self.token_file.is_none() && self.endpoint.is_none());
        if let Some(source) = source {
            return source.read().map(Some);
        }
        package
            .token_path(self.token_file.as_deref())?
            .map(|path| read_token(&path))
            .transpose()
    }
}
