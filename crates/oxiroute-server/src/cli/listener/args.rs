use clap::Subcommand;

use crate::cli::parse::nonempty_target;

#[derive(Subcommand)]
pub(crate) enum ListenerCommand {
    /// List the listeners with their protocol, bind address, state and traffic.
    List,
    /// Show one listener.
    Show {
        /// Name of the listener.
        #[arg(value_parser = nonempty_target)]
        listener: String,
    },
    /// Accept new connections on the listeners again.
    Ready {
        /// Names of the listeners.
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    /// Stop accepting new connections on the listeners; existing ones are not revoked.
    Drain {
        /// Names of the listeners.
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
    /// Put the listeners in maintenance: no new connections, existing ones are not revoked.
    Maintenance {
        /// Names of the listeners.
        #[arg(required = true, value_parser = nonempty_target)]
        listeners: Vec<String>,
    },
}
