use std::{
    thread,
    time::{Duration, Instant},
};

use super::{
    error::CliError,
    net::{Timeouts, span},
};

const POLL: Duration = Duration::from_millis(200);

pub(crate) enum Attempt<T> {
    Done(T),
    Pending(String),
}

pub(crate) fn until<T>(
    what: &str,
    timeouts: Timeouts,
    mut probe: impl FnMut() -> Result<Attempt<T>, CliError>,
) -> Result<T, CliError> {
    let deadline = Instant::now() + timeouts.wait();
    loop {
        let reason = match probe()? {
            Attempt::Done(done) => return Ok(done),
            Attempt::Pending(reason) => reason,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CliError::Timeout(format!(
                "{what} did not complete within {}: {reason}; raise the limit with --timeout",
                span(timeouts.wait())
            )));
        }
        thread::sleep(POLL.min(remaining));
    }
}

#[cfg(test)]
mod tests;
