use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum PoolCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        pool: String,
    },
    Ready {
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
    },
    Drain {
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
    },
    /// Drain every server of the pools. A drained pool has no other server to move requests to, so they
    /// keep running unless `--force` cancels them.
    Evacuate {
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
        /// Cancel the running requests; their callers get an error.
        #[arg(long)]
        force: bool,
    },
}
