use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum ListenerCommand {
    List,
    Show {
        #[arg(value_parser = nonempty_target)]
        listener: String,
    },
    Ready {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    Drain {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    Maintenance {
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
}
