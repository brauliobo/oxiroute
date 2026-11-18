use std::time::Duration;

use clap::Subcommand;

use crate::cli::parse::parse_duration;

#[derive(Subcommand)]
pub(crate) enum EventsCommand {
    /// List the management events after a cursor, oldest first.
    List {
        /// Only events after this cursor; 0 starts at the oldest retained event.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Most events to print, 1 to 1000.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    /// Print new management events as they happen until interrupted.
    ///
    /// Reads the server-sent event stream and reconnects from the last event it printed when the
    /// connection drops, giving up when the daemon stays unreachable for --timeout (30s by
    /// default). Ctrl-C ends it with status 130.
    Follow {
        /// Print the retained events after this cursor first; 0 starts at the oldest retained event.
        #[arg(long, conflicts_with = "since")]
        after: Option<u64>,
        /// Print the retained events of the last DURATION first, for example 10m.
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        since: Option<Duration>,
        /// Stop after printing this many events instead of waiting for Ctrl-C.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
        count: Option<u64>,
    },
}
