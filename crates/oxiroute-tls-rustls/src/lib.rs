//! A rustls server for connections that a Pingora listener accepted.
//!
//! [`RustlsAcceptor`] runs the handshake of an accepted socket and returns a [`TlsStream`] that is a Pingora `IO`
//! stream: it reports its TLS digest and ALPN, shares TLS records between small writes and answers readiness
//! without decrypting. Which [`rustls::ServerConfig`] serves a connection is decided by a [`ConfigRouter`] from the
//! `ClientHello`, so the listener owner keeps certificate selection, ALPN policy and challenge handling.
//!
//! A connection that is offered to the kernel (see [`RustlsAcceptor::with_kernel_tls`]) is converted to kernel TLS
//! when its HTTP layer first asks to send a file with `sendfile(2)`: rustls hands both traffic secrets over, the
//! kernel encrypts and decrypts every later record. Connections that cannot be converted stay in user space. After
//! the conversion a TLS 1.3 `KeyUpdate` or a renegotiation from the peer ends the connection, because the kernel
//! cannot follow it without the traffic secret rustls does not give out.

mod acceptor;
mod digest;
mod kernel;
mod kernel_io;
mod pump;
mod records;
mod stream;
mod user_tls;

pub use acceptor::{ConfigRouter, RustlsAcceptor};
pub use digest::openssl_suite_name;
pub use kernel::{KernelTls, KtlsUnavailable, probe};
pub use stream::TlsStream;
