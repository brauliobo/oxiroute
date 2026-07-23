# Vendored pingora-core patch

This directory contains the published `pingora-core 0.8.1` source from crates.io
(registry checksum `6a7ffe2f5acf9f94fd255cfd1438866bc9124f8f0c7d42562bd3f853df2094b7`)
under its upstream Apache-2.0 license.

OxiRoute's delta is intentionally limited to an OpenSSL-derived, per-peer TLS
configure hook, pre-handshake application admission, and a non-truncating certificate subject
conversion:

- `protocols/tls/mod.rs` declares the public `TlsConfigureHook` type.
- `upstreams/peer.rs` stores the optional hook in `PeerOptions`, omits it from
  `Debug`, and exposes it through `Peer`.
- `connectors/tls/boringssl_openssl/mod.rs` invokes it after CA, verification,
  hostname, and ALPN setup, but before clearing the OpenSSL error stack and
  starting the handshake. Hook failures become `TLSHandshakeFailure` errors.
- `apps/mod.rs` exposes an opaque connection-admission guard, and
  `services/listening.rs` acquires it after TCP accept and retains it through the handshake and
  application connection lifetime.
- `utils/tls/boringssl_openssl.rs` uses OpenSSL's non-truncating subject string
  conversion instead of the deprecated conversion that stops at interior NULs.

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, replace this directory from the newly locked published
crate, reapply and review the connector hook, admission guard, and subject conversion, then rerun
the OxiRoute TLS wire tests and strict clippy checks.
