use std::path::PathBuf;

use clap::Args;

#[derive(Args)]
pub(crate) struct DoctorArgs {
    /// Configuration file to diagnose; default: the file the systemd unit serves, else `oxiroute.kdl`.
    #[arg(value_name = "CONFIG")]
    pub(super) config: Option<PathBuf>,
    /// systemd unit to check the writable paths and capabilities of; default: `oxiroute.service` in the
    /// standard unit directories.
    #[arg(long, value_name = "FILE")]
    pub(super) unit: Option<PathBuf>,
    /// Do not ask the daemon at `--endpoint` whether it is running, so an address in use is always a problem.
    #[arg(long)]
    pub(super) offline: bool,
    /// Print one JSON object instead of the report.
    #[arg(long)]
    pub(super) json: bool,
}
