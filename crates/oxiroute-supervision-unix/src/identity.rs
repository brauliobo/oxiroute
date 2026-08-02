use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Kernel-authenticated identity of the process at the other end of a Unix socket.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PeerIdentity {
    pid: i32,
    uid: u32,
    gid: u32,
}

impl PeerIdentity {
    pub(crate) fn from_credentials(credentials: rustix::net::UCred) -> Self {
        Self {
            pid: credentials.pid.as_raw_pid(),
            uid: credentials.uid.as_raw(),
            gid: credentials.gid.as_raw(),
        }
    }

    /// Returns the peer process ID.
    #[must_use]
    pub const fn pid(self) -> i32 {
        self.pid
    }

    /// Returns the peer effective user ID.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the peer effective group ID.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

/// Private capability exchanged by a parent and its newly started child.
///
/// Entropy generation is intentionally left to the spawning layer, which is outside this crate.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpawnHandshakeNonce([u8; 32]);

impl SpawnHandshakeNonce {
    /// Wraps 256 bits supplied by the spawning layer.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Exposes the bytes for the private parent-child handshake message.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SpawnHandshakeNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SpawnHandshakeNonce([REDACTED])")
    }
}

impl Drop for SpawnHandshakeNonce {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
