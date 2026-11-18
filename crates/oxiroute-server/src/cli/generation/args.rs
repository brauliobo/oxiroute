use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum GenerationCommand {
    Status,
    /// Apply the configuration file on disk as a new generation.
    Reload {
        /// Wait until the new revision is active or the file is rejected, up to --timeout (30s by default).
        #[arg(long)]
        wait: bool,
    },
    Rollback,
    Drain {
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=300_000))]
        timeout_ms: u64,
    },
}
