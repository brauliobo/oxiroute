use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProcessCommand {
    Status,
    Ready,
    Metrics,
    Topology,
    Monitoring,
    Drain,
    Shutdown,
}
