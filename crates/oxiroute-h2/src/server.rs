//! The async HTTP/2 server: [`Server`] drives a [`Connection`](crate::conn::Connection) over any
//! `AsyncRead + AsyncWrite` transport and hands out one [`Stream`] per request.
//!
//! The connection makes progress only while [`Server::accept`] is being polled: it reads, parses, writes and
//! flushes, runs the SETTINGS acknowledgement timer and answers control frames. The proxy applies its own read
//! and idle timeouts on the application side (this crate has no idle timer beyond the SETTINGS acknowledgement).
//! Streams share the connection through one mutex that is never held across a suspension point.
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::conn::Connection;

mod buffer;
mod config;
mod driver;
mod stream;

pub use config::Config;
pub use driver::Server;
pub use stream::Stream;

/// The connection state shared by the driver and every stream handle.
pub(crate) struct Shared {
    conn: Mutex<Connection>,
}

impl Shared {
    fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// A panic while the lock was held cannot leave the state half-updated in a way later calls can observe
    /// more harmfully than by failing, so poisoning is ignored.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
