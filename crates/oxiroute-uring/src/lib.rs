//! A narrow, safe `io_uring` API for the HTTP/1 data plane.
//!
//! This is the only crate of the workspace that is permitted to contain `unsafe`, and all of it is in `raw`
//! (the workspace rule `unsafe_code = "forbid"` stays in force for every other crate; `scripts/verify/unsafe_containment.sh`
//! enforces that). The rest of the crate is safe code over that module.
//!
//! The API is owned-buffer and completion based. A [`Ring`] adopts sockets and answers with plain-data
//! [`Event`]s; receives land in a shared provided-buffer ring and are copied out with [`Ring::read`], sends are
//! staged into buffers the ring owns, and closing a socket with operations in flight is the ring's problem: it
//! cancels what can be cancelled, lets an accepted send finish and absorbs every late completion.
#![deny(unsafe_op_in_unsafe_fn)]

mod addr;
mod config;
mod event;
mod probe;
mod raw;
mod ring;
mod slot;

pub use config::{Config, Taskrun};
pub use event::{Event, Read, Released, Sent};
pub use probe::{Missing, probe};
pub use ring::Ring;
pub use slot::Handle;
