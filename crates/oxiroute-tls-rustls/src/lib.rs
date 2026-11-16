//! A rustls server for connections that a Pingora listener accepted.
//!
//! [`RustlsAcceptor`] runs the handshake of an accepted socket and returns a [`TlsStream`] that is a Pingora `IO`
//! stream: it reports its TLS digest and ALPN, shares TLS records between small writes and answers readiness
//! without decrypting. Which [`rustls::ServerConfig`] serves a connection is decided by a [`ConfigRouter`] from the
//! `ClientHello`, so the listener owner keeps certificate selection, ALPN policy and challenge handling.
//!
//! A connection that is offered to the kernel (see [`RustlsAcceptor::with_kernel_tls`]) is converted to kernel TLS
//! when its HTTP layer first asks to send a file with `sendfile(2)`: rustls hands both traffic secrets over, the
//! kernel encrypts and decrypts every later record. Connections that cannot be converted stay in user space.
//!
//! The kernel never changes keys by itself, so the converted connection does: a TLS 1.3 `KeyUpdate` of the peer is
//! followed with the next receive key (and answered with one of ours when it asks for it), and the key it sends with
//! rolls before the AES-GCM confidentiality limit. The next keys derive from the traffic secrets that the handshake
//! reported to a key log of the connection ([`KernelTls`] is offered only for TLS 1.3 when the kernel changes keys of
//! a running connection, and [`probe`] checks that). A TLS 1.2 connection cannot change keys: it ends at the AES-GCM
//! record limit, like rustls does, and a renegotiation request of the peer ends it.

mod acceptor;
mod digest;
mod kernel;
mod pump;
mod records;
mod stream;
mod user_tls;

pub use acceptor::{ConfigRouter, RustlsAcceptor};
pub use digest::openssl_suite_name;
pub use kernel::{KernelTls, KtlsUnavailable, probe};
pub use stream::TlsStream;
