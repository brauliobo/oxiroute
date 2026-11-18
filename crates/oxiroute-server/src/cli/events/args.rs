use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum EventsCommand {
    List {
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    Follow {
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        #[arg(long, default_value_t = 1_000)]
        interval_ms: u64,
    },
}
