use std::{ffi::OsString, process::ExitCode};

use clap::{CommandFactory as _, Parser};

use command::Command;
use context::Context;
pub use context::ServeFn;
use error::CliError;
use global::Global;

mod binary;
mod change;
mod check;
mod command;
mod completions;
mod config;
mod context;
mod contexts;
mod error;
mod events;
mod fs;
mod generation;
mod global;
mod help;
mod import;
mod listener;
mod man;
mod net;
mod overlay;
mod parse;
mod plugin;
mod pool;
mod process;
mod registry;
mod render;
mod requests;
mod rtmp;
mod serve;
mod server;
mod shorthand;
mod tls;
mod top;
mod units;
mod version;
mod view;
mod wait;

#[cfg(test)]
mod tests;

pub(crate) const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "oxiroute",
    version = BUILD_VERSION,
    about = help::ABOUT,
    long_about = help::LONG_ABOUT,
    after_help = help::GETTING_STARTED,
    after_long_help = help::OVERVIEW,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    #[must_use]
    pub fn parse_process() -> Self {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        match Self::try_parse_process_from(&arguments) {
            Ok(cli) => cli,
            Err(error) if error.use_stderr() => match plugin::invoked(&arguments) {
                Some(command) => {
                    println!("{}", plugin::usage_line(&command, &error));
                    std::process::exit(i32::from(plugin::EXIT_UNKNOWN));
                }
                None => error.exit(),
            },
            Err(error) => error.exit(),
        }
    }

    pub(crate) fn try_parse_process_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let arguments = arguments.into_iter().map(Into::into).collect();
        Self::try_parse_from(shorthand::expand(arguments, &Self::command()))
    }

    pub(crate) fn subcommand(&self) -> &Command {
        &self.command
    }

    #[must_use]
    pub fn execute(&self, serve: ServeFn) -> ExitCode {
        let context = Context::new(&self.global, serve);
        match self.subcommand().run(&context) {
            Ok(code) => code,
            Err(error) => {
                if !matches!(error, CliError::OutputClosed | CliError::Differs) {
                    render::diagnose(&format!("oxiroute: {error}"));
                }
                ExitCode::from(error.exit_code())
            }
        }
    }
}
