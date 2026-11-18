use std::ffi::OsString;

use clap::CommandFactory;

use crate::cli::Cli;

/// The plugin line for a usage error: the first line of clap's message, since the line is one line.
pub(in crate::cli) fn usage_line(error: &clap::Error) -> String {
    let message = error.render().to_string();
    let reason = message.lines().next().unwrap_or_default();
    let reason = reason.strip_prefix("error: ").unwrap_or(reason);
    format!("OXIROUTE UNKNOWN - {reason} (see `oxiroute check --help`)")
}

/// Whether the arguments name `check`, even when they are otherwise invalid.
pub(in crate::cli) fn is_invocation(arguments: &[OsString]) -> bool {
    <Cli as CommandFactory>::command()
        .ignore_errors(true)
        .try_get_matches_from(arguments)
        .is_ok_and(|matches| matches.subcommand_name() == Some("check"))
}
