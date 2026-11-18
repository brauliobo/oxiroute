use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use oxiroute_config_source::ConfigFormat;

#[derive(Subcommand)]
pub(crate) enum ConfigCommand {
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
