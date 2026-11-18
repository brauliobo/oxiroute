use std::path::PathBuf;

use clap::{Args, ValueEnum};
use zeroize::Zeroizing;

use crate::cli::{
    error::CliError,
    net::{DEFAULT_ENDPOINT, ENDPOINT_ENV, Endpoint, MANAGEMENT_TOKEN_ENV, Package, read_token},
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
            then the management bind of /etc/oxiroute/oxiroute.kdl or .lua, then uses http://127.0.0.1:9900."
    )]
    pub(super) endpoint: Option<String>,
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
            /etc/oxiroute/oxiroute.kdl or .lua, then /etc/oxiroute/management.token when it exists."
    )]
    pub(super) token_file: Option<PathBuf>,
    /// Output format of results.
    #[arg(long, global = true, value_enum, default_value_t = Output::Table, value_name = "FORMAT")]
    pub(super) output: Output,
    /// Suppress normal output; errors still go to stderr and the exit code is unchanged.
    #[arg(long, global = true)]
    pub(super) quiet: bool,
}

impl Global {
    pub(super) fn endpoint(&self, package: &Package) -> Result<Endpoint, CliError> {
        if let Some(path) = &self.socket {
            return Endpoint::unix(&path.to_string_lossy());
        }
        let endpoint = self
            .endpoint
            .clone()
            .or_else(|| package.discovered_endpoint())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        Endpoint::parse(&endpoint)
    }

    pub(super) fn token(&self, package: &Package) -> Result<Option<Zeroizing<String>>, CliError> {
        package
            .token_path(self.token_file.as_deref())?
            .map(|path| read_token(&path))
            .transpose()
    }
}
