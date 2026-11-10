//! A thin HTTP/3 server layer (RFC 9114) directly over `quinn` streams, with QPACK (RFC 9204) that needs no
//! dynamic table.
//!
//! * [`server::Connection`] opens the control stream, accepts request streams and reads the peer's control, QPACK
//!   and unknown streams while it waits for them.
//! * [`server::RequestStream`] is owned by whoever serves the request: head, body and trailers in, response out.
//! * [`qpack`], [`frame`], [`varint`], [`head`] and [`parse`] are the codecs and parsers, usable without I/O.

pub mod code;
mod error;
pub mod frame;
pub mod head;
pub mod parse;
pub mod qpack;
pub mod server;
pub mod varint;

pub use code::Code;
pub use error::{Error, HeadError};
