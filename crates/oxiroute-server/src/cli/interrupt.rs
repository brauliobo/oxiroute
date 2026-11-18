use std::{process, thread};

use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};

use super::error::CliError;

/// Ends a command that runs until it is stopped at the first SIGINT or SIGTERM, with the status of a process the
/// signal ended (130 and 143), after the lines it printed are flushed.
pub(super) fn exit_when_signalled() -> Result<(), CliError> {
    let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(CliError::local)?;
    thread::Builder::new()
        .name("interrupt".into())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                process::exit(128 + signal);
            }
        })
        .map_err(CliError::local)?;
    Ok(())
}
