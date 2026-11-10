//! Purpose-built HTTP/2 server session (RFC 9113 + RFC 7541) for the proxy subset.
//!
//! * [`hpack`]: header compression, independent of the rest.
//! * [`frame`] and [`conn`]: frame codec, connection/stream state machine, flow control, abuse limits.
//! * [`server`]: the async driver and the stream handles an application (the Pingora adapter) uses.

// Sizes and windows are bounded by the protocol (frames below 2^24, windows below 2^31), so the casts between
// the integer widths used for them cannot lose information.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

pub mod conn;
mod error;
pub mod frame;
pub mod hpack;
pub mod server;

pub use error::{Error, Initiator, Reason};
