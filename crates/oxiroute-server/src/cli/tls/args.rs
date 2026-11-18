use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum TlsCommand {
    /// List the certificates with their source, expiry and last renewal outcome.
    List,
    /// Reload certificate files from disk now instead of waiting for the watcher.
    Reconcile {
        /// Only this certificate; default: every certificate.
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
}
