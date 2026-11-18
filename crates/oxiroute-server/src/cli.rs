use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::Parser;

use command::Command;
use context::Context;
pub use context::ServeFn;
use error::CliError;
use global::Global;

mod check;
mod command;
mod config;
mod context;
mod error;
mod events;
mod fs;
mod generation;
mod global;
mod import;
mod listener;
mod net;
mod parse;
mod pool;
mod process;
mod registry;
mod render;
mod requests;
mod rtmp;
mod serve;
mod server;
mod tls;
mod top;
mod version;

#[cfg(test)]
mod tests;

pub(crate) const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "oxiroute", version = BUILD_VERSION)]
pub struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Option<Command>,

    /// Shorthand for `serve CONFIG` when no subcommand is present.
    #[arg(value_name = "CONFIG")]
    legacy_config: Option<PathBuf>,
}

impl Cli {
    #[must_use]
    pub fn parse_process() -> Self {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        match Self::try_parse_process_from(&arguments) {
            Ok(cli) => cli,
            Err(error) if error.use_stderr() && check::is_invocation(&arguments) => {
                println!("{}", check::usage_line(&error));
                std::process::exit(i32::from(check::EXIT_UNKNOWN));
            }
            Err(error) => error.exit(),
        }
    }

    pub(crate) fn try_parse_process_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::try_parse_from(arguments).map(Self::normalize)
    }

    fn normalize(mut cli: Self) -> Self {
        if cli.command.is_none() {
            let config = cli.legacy_config.take();
            cli.command = Some(Command::Serve(serve::ServeArgs::new(config)));
        }
        cli
    }

    pub(crate) fn command(&self) -> &Command {
        self.command
            .as_ref()
            .expect("CLI parsing always resolves a command")
    }

    #[must_use]
    pub fn execute(&self, serve: ServeFn) -> ExitCode {
        let context = Context::new(&self.global, serve);
        match self.command().run(&context) {
            Ok(code) => code,
            Err(error) => {
                if !self.global.quiet || matches!(error, CliError::Startup(_)) {
                    eprintln!("{}: {error}", error.prefix());
                }
                ExitCode::from(error.exit_code())
            }
        }
    }
}
