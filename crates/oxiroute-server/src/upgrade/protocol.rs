use std::{
    fmt::Write as _,
    io,
    os::{linux::net::SocketAddrExt as _, unix::ffi::OsStrExt as _, unix::net::SocketAddr},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const NAME_PREFIX: &str = "oxiroute-upgrade-";
const NAME_DIGEST_BYTES: usize = 16;

/// The abstract socket on which the master that serves `config` accepts upgrade requests.
///
/// It is named after the resolved configuration path, so that every master on a host has its own
/// and a client finds the master of the configuration it names.
///
/// # Errors
///
/// Returns an error when `config` cannot be resolved.
pub fn control_address(config: &Path) -> io::Result<SocketAddr> {
    let resolved = std::fs::canonicalize(config)?;
    let digest = Sha256::digest(resolved.as_os_str().as_bytes());
    let mut name = String::from(NAME_PREFIX);
    for byte in &digest[..NAME_DIGEST_BYTES] {
        write!(name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    SocketAddr::from_abstract_name(name)
}

/// What the client asks of the master.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Command {
    /// The executable to hand over to; the master's own path when absent.
    pub binary: Option<PathBuf>,
}

/// What the master reports while it hands over, one JSON object per line.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum Event {
    /// The successor master is starting from `binary`.
    Starting {
        binary: PathBuf,
        build_id: String,
        previous_build_id: String,
    },
    /// The successor serves the listeners; the previous master's worker drains what it holds.
    Serving { pid: u32 },
    /// The previous master's worker is gone.
    Drained,
    /// The master serves another user and does not take requests from this one.
    Refused,
    /// The hand-over did not happen, and the previous master keeps serving.
    Failed { reason: String },
}
