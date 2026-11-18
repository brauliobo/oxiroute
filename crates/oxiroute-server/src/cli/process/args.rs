use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProcessCommand {
    Status,
    /// Succeed when the daemon accepts traffic.
    Ready {
        /// Keep asking until the daemon is ready, up to --timeout (30s by default).
        #[arg(long)]
        wait: bool,
    },
    Metrics,
    Topology,
    Monitoring,
    Drain,
    Shutdown,
}
