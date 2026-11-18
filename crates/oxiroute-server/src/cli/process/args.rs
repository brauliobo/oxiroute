use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProcessCommand {
    /// Show the daemon's build, health, active revision and listeners.
    Status,
    /// Succeed when the daemon accepts traffic.
    #[command(after_help = "Example:\n  oxiroute ready --wait --timeout 60s && echo up")]
    Ready {
        /// Keep asking until the daemon is ready, up to --timeout (30s by default).
        #[arg(long)]
        wait: bool,
    },
    /// Print the Prometheus metrics exactly as the daemon serves them.
    Metrics,
    /// Show how listeners, services, routes and pools of the active configuration connect.
    Topology,
    /// Show process, host and traffic figures with the state of listeners and pools.
    Monitoring,
    /// Refuse new admissions and make `ready` fail while running work finishes.
    Drain,
    /// Stop the daemon gracefully, within the five-second shutdown deadline.
    Shutdown,
}
