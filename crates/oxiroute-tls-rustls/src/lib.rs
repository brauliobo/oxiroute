//! A rustls server for connections that a Pingora listener accepted.
//!
//! [`RustlsAcceptor`] runs the handshake of an accepted socket and returns a [`TlsStream`] that is a Pingora `IO`
//! stream: it reports its TLS digest and ALPN, shares TLS records between small writes and answers readiness
//! without decrypting. Which [`rustls::ServerConfig`] serves a connection is decided by a [`ConfigRouter`] from the
//! `ClientHello`, so the listener owner keeps certificate selection, ALPN policy and challenge handling.

mod acceptor;
mod digest;
mod pump;
mod stream;

pub use acceptor::{ConfigRouter, RustlsAcceptor};
pub use digest::openssl_suite_name;
pub use stream::TlsStream;
