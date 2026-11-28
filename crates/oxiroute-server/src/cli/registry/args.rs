use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum RegistryCommand {
    /// List all active dynamic server registrations, leases and registry status.
    List,
    /// Create scoped tokens for registering servers or reading the registry.
    Token {
        #[command(subcommand)]
        command: RegistryTokenCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum RegistryTokenCommand {
    /// Append a scoped token to a tokens file and print its secret once.
    Create {
        /// Tokens file to append the token to.
        #[arg(long, value_name = "FILE")]
        tokens_file: PathBuf,
        /// Identifier of the token in the tokens file.
        #[arg(long, value_parser = nonempty_target)]
        id: String,
        /// Pool the token may register servers in; repeat for several.
        #[arg(long = "pool", required_unless_present = "read", value_parser = nonempty_target)]
        pools: Vec<String>,
        /// Only server names starting with this prefix may be registered.
        #[arg(long, requires = "pools")]
        name_prefix: Option<String>,
        /// Create a read-only token instead.
        #[arg(long, conflicts_with = "pools")]
        read: bool,
    },
}
