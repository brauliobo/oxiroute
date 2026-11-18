use clap::Subcommand;

use crate::cli::parse::{nonempty_target, parse_method};

#[derive(Subcommand)]
pub(crate) enum RouteCommand {
    /// Ask the daemon which route, action and pool would serve a request, without sending it.
    ///
    /// The request goes through the route table of the active generation, the one that serves traffic,
    /// on every HTTP listener or only on --listener. Give a URL, or a host and a path. Exits 6 when no
    /// route serves it.
    Test {
        /// URL such as <https://example.com/api/users>, or the host as the request names it when PATH follows.
        #[arg(value_name = "URL|HOST", value_parser = nonempty_target)]
        target: String,
        /// Path of the request, starting with `/`, when the first argument is a host.
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// Method of the request.
        #[arg(long, default_value = "GET", value_parser = parse_method)]
        method: String,
        /// Only this HTTP listener.
        #[arg(long, value_parser = nonempty_target)]
        listener: Option<String>,
    },
}
