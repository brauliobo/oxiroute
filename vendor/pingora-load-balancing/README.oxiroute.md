# Vendored pingora-load-balancing

This directory contains the published `pingora-load-balancing 0.9.1` source from crates.io
(registry checksum `cbd4318f1349222fd355029ba59fc387cb955b3ed5c0411137e3c0ee0486b481`)
under its upstream Apache-2.0 license. It is vendored because the product's health checks use it and
its tests must not need the Internet.

OxiRoute's delta:

- `health_check.rs`: `TcpHealthCheck` and `HttpHealthCheck` hold an `Arc` connector and default to one
  process-wide connector (probes use fresh connections, so no connection state is shared between pools);
  `set_connector` accepts an owned connector or an `Arc`.
- `Cargo.toml` has no `rustls` feature (`pingora-rustls` is no longer vendored) and no `derivative`
  dependency.
- `lib.rs` and `test_servers.rs`: the tests run against loopback servers that own a kernel-assigned port
  instead of Internet hosts.

Upgrade by importing the published crate as its own commit and re-applying the delta with a three-way
merge from the previous release; `scripts/test-vendored.sh load-balancing` runs its suites.
