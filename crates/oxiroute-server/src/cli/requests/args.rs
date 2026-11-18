use clap::Subcommand;

use crate::cli::parse::parse_age_ms;

#[derive(Subcommand)]
pub(crate) enum RequestsCommand {
    /// List live requests, oldest first.
    List {
        #[arg(long)]
        pool: Option<String>,
        #[arg(long)]
        route: Option<String>,
        /// Only requests older than this, for example 500ms, 30s, 2m or 1h.
        #[arg(long, value_parser = parse_age_ms)]
        older_than: Option<u64>,
        /// One of `routed`, `queued`, `connecting`, `awaiting_headers`, `streaming`, `retry_wait`.
        #[arg(long)]
        phase: Option<String>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    /// Show one live request by its request id.
    Show {
        #[arg(value_name = "REQUEST_ID")]
        id: String,
    },
}
