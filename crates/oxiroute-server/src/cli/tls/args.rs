use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum TlsCommand {
    List,
    Reconcile {
        #[arg(long, value_parser = nonempty_target)]
        certificate: Option<String>,
    },
}
