use std::{path::PathBuf, time::Duration};

use clap::{Args, Subcommand};

use crate::cli::parse::{nonempty_target, parse_timeout};

#[derive(Subcommand)]
pub(crate) enum ContextCommand {
    /// List the named contexts with their address, token source and default timeout.
    List,
    /// Add a named context, or replace one with `--replace`.
    Add(AddArgs),
    /// Make a context the one later commands use when no flag or variable names another.
    Use {
        /// Name of the context.
        #[arg(value_parser = nonempty_target)]
        name: String,
    },
    /// Remove a context; the token source it names is left alone.
    Remove {
        /// Name of the context.
        #[arg(value_parser = nonempty_target)]
        name: String,
    },
    /// Print the name of the context the next command uses.
    Current,
}

#[derive(Args)]
pub(crate) struct AddArgs {
    /// Name of the context: letters, digits, `.`, `_` and `-`.
    #[arg(value_parser = nonempty_target)]
    pub(super) name: String,
    /// Management API URL of the daemon, `http://HOST:PORT` or `unix:/path/to/socket`.
    #[arg(long, value_name = "URL")]
    pub(super) address: String,
    /// Read the token from this file (regular file, mode 0400 or 0600, absolute path).
    #[arg(long, value_name = "FILE", group = "source")]
    pub(super) token_from_file: Option<PathBuf>,
    /// Read the token from this environment variable at each use.
    #[arg(long, value_name = "VARIABLE", group = "source")]
    pub(super) token_from_env: Option<String>,
    /// Read the token from the output of this command, run without a shell; it takes every word after it.
    #[arg(
        long,
        value_name = "PROGRAM",
        num_args = 1..,
        allow_hyphen_values = true,
        group = "source"
    )]
    pub(super) token_from_command: Option<Vec<String>>,
    /// Time limit for connecting and for each answer when `--timeout` is not given, such as 15s.
    #[arg(long, value_parser = parse_timeout, value_name = "DURATION")]
    pub(super) default_timeout: Option<Duration>,
    /// Make this the context later commands use.
    #[arg(long = "use")]
    pub(super) make_current: bool,
    /// Replace a context of the same name.
    #[arg(long)]
    pub(super) replace: bool,
}
