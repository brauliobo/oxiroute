use clap::Subcommand;

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
    /// Keep printing new management events as they happen until interrupted.
    Follow {
        /// Start after this cursor; 0 starts at the oldest retained event.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Most events to fetch per poll, 1 to 1000.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        /// Milliseconds between polls.
        #[arg(long, default_value_t = 1_000)]
        interval_ms: u64,
    },
}
