use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum GenerationCommand {
    /// Show the active, candidate and previous revisions and the generation counters.
    Status,
    /// Apply the configuration file on disk as a new generation.
    Reload {
        /// Wait until the new revision is active or the file is rejected, up to --timeout (30s by default).
        #[arg(long)]
        wait: bool,
    },
    /// Activate the previous generation again.
    Rollback,
    /// Close the generation's admission gate and report what still holds it.
    Drain {
        /// Deadline for the drain in milliseconds, 0 to 300000.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(..=300_000))]
        timeout_ms: u64,
    },
}
