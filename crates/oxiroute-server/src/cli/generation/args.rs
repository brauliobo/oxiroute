use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum GenerationCommand {
    Status,
    Reload,
    Rollback,
    Drain {
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=300_000))]
        timeout_ms: u64,
    },
}
