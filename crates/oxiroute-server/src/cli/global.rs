use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use zeroize::Zeroizing;

use crate::cli::{
    error::CliError,
    net::{
        DEFAULT_MANAGEMENT_TOKEN_FILE, DEFAULT_PACKAGE_ENV_FILE, Endpoint, MANAGEMENT_TOKEN_ENV,
        read_token, resolve_token_path,
    },
};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9900";

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Output {
    Json,
    Table,
    Plain,
}

#[derive(Args)]
pub(crate) struct Global {
    #[arg(long, env = "OXIROUTE_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    pub(super) endpoint: String,
    /// Talk to a registry unix socket instead of the management API; no token is used.
    #[arg(long, value_name = "PATH")]
    pub(super) socket: Option<PathBuf>,
    #[arg(long)]
    pub(super) token_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = Output::Table)]
    pub(super) output: Output,
    #[arg(long)]
    pub(super) quiet: bool,
}

impl Global {
    pub(super) fn endpoint(&self) -> Result<Endpoint, CliError> {
        match &self.socket {
            Some(path) => Endpoint::unix(&path.to_string_lossy()),
            None => Endpoint::parse(&self.endpoint),
        }
    }

    pub(super) fn token(&self) -> Result<Option<Zeroizing<String>>, CliError> {
        let configured = std::env::var_os(MANAGEMENT_TOKEN_ENV);
        resolve_token_path(
            self.token_file.as_deref(),
            configured.as_deref().map(Path::new),
            Path::new(DEFAULT_PACKAGE_ENV_FILE),
            Path::new(DEFAULT_MANAGEMENT_TOKEN_FILE),
        )?
        .map(|path| read_token(&path))
        .transpose()
    }
}
