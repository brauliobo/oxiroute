//! The wire parsers without I/O: what a request stream and the peer's control stream carry. The server drives them
//! with the bytes quinn delivers; tests and fuzz targets drive them from memory.
pub mod control;
pub mod request;

pub use control::{Control, Violation};
pub use request::{Event, Fault, Phase, RequestParser};
