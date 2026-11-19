use std::{
    io::{self, BufRead as _, BufReader, Write as _},
    os::unix::net::UnixStream,
    path::Path,
};

use thiserror::Error;

use super::{Command, Event, control_address};

#[derive(Debug, Error)]
pub enum UpgradeError {
    #[error("cannot resolve the configuration `{path}`: {source}")]
    Config { path: String, source: io::Error },
    #[error(
        "no oxiroute master serves `{path}`; name the configuration the daemon runs with with \
         --config. A daemon that runs without the supervising master has nothing to hand over from"
    )]
    NotRunning { path: String },
    #[error("the master of `{path}` refused the request: it runs as another user")]
    Refused { path: String },
    #[error("the master's answer could not be read: {0}")]
    Io(#[from] io::Error),
    #[error("the master closed the connection before the hand-over ended")]
    Interrupted,
    #[error("{reason}")]
    Failed { reason: String },
}

/// Asks the master of `config` to hand over to `command.binary` and reports every stage to
/// `on_event`.
///
/// It returns once the successor serves, or with `wait` once the previous worker has drained too.
///
/// # Errors
///
/// Returns an error when no master answers, the master refuses, or the hand-over fails.
pub fn request(
    config: &Path,
    command: &Command,
    wait: bool,
    mut on_event: impl FnMut(&Event),
) -> Result<(), UpgradeError> {
    let path = config.display().to_string();
    let address = control_address(config).map_err(|source| UpgradeError::Config {
        path: path.clone(),
        source,
    })?;
    let mut stream = UnixStream::connect_addr(&address).map_err(|source| match source.kind() {
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
            UpgradeError::NotRunning { path: path.clone() }
        }
        _ => UpgradeError::Io(source),
    })?;
    let mut line = serde_json::to_vec(command).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let event: Event = serde_json::from_str(&line).map_err(io::Error::other)?;
        on_event(&event);
        match event {
            Event::Refused => return Err(UpgradeError::Refused { path }),
            Event::Failed { reason } => return Err(UpgradeError::Failed { reason }),
            Event::Serving { .. } if !wait => return Ok(()),
            Event::Drained => return Ok(()),
            Event::Starting { .. } | Event::Serving { .. } => {}
        }
    }
    Err(UpgradeError::Interrupted)
}
