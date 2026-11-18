use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use oxiroute_config_source::ConfigFormat;

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
    /// Check a configuration file offline: parse, validate and prepare it without a daemon.
    Check {
        /// Configuration file in a native or canonical format.
        config: PathBuf,
    },
    /// Compose finalized canonical configurations in the supplied order.
    Compose {
        /// Format to print the composed configuration in.
        #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
        format: ComposeFormat,
        /// Finalized canonical configuration files, in the order they apply.
        #[arg(value_name = "CONFIG", required = true, num_args = 1..)]
        configs: Vec<PathBuf>,
    },
    /// Show the active configuration's format, revisions and number of entries per section.
    Get,
    /// Ask the daemon to validate and prepare a canonical JSON configuration without publishing it.
    Validate {
        /// Canonical configuration as JSON, or the `config` field of `config get --output json`.
        file: PathBuf,
    },
    /// Replace the configuration on disk with a canonical JSON configuration and reload it.
    Apply {
        /// Canonical configuration as JSON, or the `config` field of `config get --output json`.
        file: PathBuf,
    },
    /// Tell whether a canonical JSON configuration differs from the active one.
    Diff {
        /// Canonical configuration as JSON, or the `config` field of `config get --output json`.
        file: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ComposeFormat {
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
