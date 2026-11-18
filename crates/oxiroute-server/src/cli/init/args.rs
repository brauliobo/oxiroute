use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::cli::config::ComposeFormat;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Kind {
    /// An HTTP reverse proxy to one upstream pool.
    Proxy,
    /// An HTTP server of static files.
    Static,
    /// An RTMP ingest and playback server.
    Rtmp,
}

#[derive(Args)]
pub(crate) struct InitArgs {
    /// What the configuration serves.
    #[arg(value_enum)]
    pub(super) kind: Kind,
    /// Syntax to write the configuration in.
    #[arg(long, value_enum, default_value_t = ComposeFormat::Kdl)]
    pub(super) format: ComposeFormat,
    /// File to write, or `-` to print the configuration; default `oxiroute.` and the format's extension.
    #[arg(long, value_name = "FILE")]
    pub(super) file: Option<PathBuf>,
    /// Replace the file when it exists.
    #[arg(long)]
    pub(super) force: bool,
}
