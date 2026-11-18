use clap::Subcommand;

use crate::cli::{change::Persist, parse::nonempty_target};

#[derive(Subcommand)]
pub(crate) enum PoolCommand {
    /// List the pools with their algorithm, state and available servers.
    List,
    /// Show one pool and its servers.
    Show {
        /// Name of the pool.
        #[arg(value_parser = nonempty_target)]
        pool: String,
    },
    /// Return every server of the pools to service; the opposite of `drain`.
    #[command(visible_alias = "undrain")]
    Ready {
        /// Names of the pools.
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
        #[command(flatten)]
        persist: Persist,
    },
    /// Stop sending new requests to every server of the pools; running ones finish.
    Drain {
        /// Names of the pools.
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
        #[command(flatten)]
        persist: Persist,
    },
    /// Drain every server of the pools. A drained pool has no other server to move requests to, so they
    /// keep running unless `--force` cancels them.
    Evacuate {
        /// Names of the pools.
        #[arg(required = true, value_parser = nonempty_target)]
        pools: Vec<String>,
        /// Cancel the running requests; their callers get an error.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        persist: Persist,
    },
}
