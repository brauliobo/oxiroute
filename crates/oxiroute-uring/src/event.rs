use std::{net::SocketAddr, os::fd::OwnedFd};

/// What a socket can report. Every event names the `tag` given when the socket was adopted.
#[derive(Debug)]
pub enum Event {
    /// Received bytes wait to be taken with `Ring::read`.
    Readable { tag: u64 },
    /// The peer closed its sending side; `Ring::read` returns the remaining bytes, then `Read::Eof`.
    Eof { tag: u64 },
    /// The socket failed; `Ring::read` and `Ring::send` report it.
    Failed { tag: u64 },
    /// A send that was refused or accepted in part can be retried.
    Writable { tag: u64 },
    /// A connect finished successfully.
    Connected { tag: u64 },
    /// A listener accepted a connection.
    Accepted {
        tag: u64,
        fd: OwnedFd,
        peer: Option<SocketAddr>,
    },
    /// The descriptor armed with `Ring::notify` became readable.
    Notified { tag: u64 },
}

impl Event {
    #[must_use]
    pub const fn tag(&self) -> u64 {
        match self {
            Self::Readable { tag }
            | Self::Eof { tag }
            | Self::Failed { tag }
            | Self::Writable { tag }
            | Self::Connected { tag }
            | Self::Accepted { tag, .. }
            | Self::Notified { tag } => *tag,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Read {
    Data(usize),
    Eof,
    Block,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sent {
    /// The bytes were staged; the ring completes the write on its own.
    Queued(usize),
    /// A previous send is still in flight; `Event::Writable` follows when it ends.
    Busy,
    Failed,
}
