use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum RegistryCommand {
    Token {
        #[command(subcommand)]
        command: RegistryTokenCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum RegistryTokenCommand {
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
