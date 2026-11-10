//! The server: [`Connection`] accepts [`RequestStream`]s.
mod config;
mod connection;
mod peer;
mod recv;
mod request;
mod scratch;
mod send;
mod shared;

pub use config::{Config, HEADERS_FRAME_FACTOR};
pub use connection::Connection;
pub use request::RequestStream;
