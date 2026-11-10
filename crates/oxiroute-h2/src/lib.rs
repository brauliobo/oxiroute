//! Purpose-built HTTP/2 server session (RFC 9113 + RFC 7541) for the proxy subset.
//!
//! * [`hpack`]: header compression, independent of the rest.
//! * [`frame`] and [`conn`]: frame codec, connection/stream state machine, flow control, abuse limits.
//! * [`server`]: the async driver and the stream handles an application (the Pingora adapter) uses.

pub mod hpack;
