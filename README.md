# OxiRoute

OxiRoute is a pre-alpha Rust proxy and traffic-routing project built on
[Cloudflare Pingora](https://github.com/cloudflare/pingora). The long-term goal is one
auditable control plane for HTTP proxying, layer-4 relays, load balancing, configuration
imports, and operational visibility.

The current code is intentionally much smaller than that goal. It provides:

- A restricted Lua configuration loader with strict typed validation.
- Pingora-backed HTTP reverse proxy listeners with deterministic host/path/method routing,
  static round-robin pools, bounded safe connect-failure retries, upstream I/O timeouts, body
  limits, and connection caps.
- HTTP/1.1 WebSocket upgrade proxying with bidirectional frame integration coverage.
- Pingora-backed opaque TCP relays with round-robin pools, connection caps, and configurable
  connect, idle, and lifetime timeouts.
- An nginx syntax parser and exact nginx-rtmp registry for all 117 active directive keys with context, arity, value, default, and runtime-status metadata.
- RTMP live publishing with simple/complex handshakes, AMF connect/createStream/publish handling, duplicate-publisher rejection, media observations, and FFmpeg interoperability.
- Immutable RTMP active-stream snapshots and a capability-gated manual recording state machine.
- A loopback-only Pingora management API for runtime monitoring, RTMP stream visibility, and exact-ID recording controls.
- A responsive Vue/Pug runtime observatory for host/process load, listener traffic, and live RTMP state.
- Acceptance tests for configuration isolation and runtime planning.

It does not yet provide forward proxying, `CONNECT`, TLS, HTTP/2 listener configuration,
HTTP/3, UDP, caching, health-aware or weighted load balancing, hot reload, full configuration
imports, RTMP playback, media fanout, or recording.
It is not a firewall, NAT implementation, or drop-in replacement for Squid, nginx,
HAProxy, or Apache httpd.

## Run

Start an upstream HTTP server on `127.0.0.1:3000`, then run:

```sh
pnpm --dir ui install
pnpm --dir ui build
cargo run -p oxiroute-server -- oxiroute.example.lua
```

The example exposes the HTTP upstream on `127.0.0.1:8080`, defines a TCP relay from
`127.0.0.1:15432` to `127.0.0.1:5432`, and accepts RTMP publishers on
`rtmp://127.0.0.1:1935/<application>/<stream>`. Runtime monitoring and RTMP status are available at
`http://127.0.0.1:9080/api/v1/monitoring` and `http://127.0.0.1:9080/api/v1/rtmp/streams`.
The Vue/Pug runtime observatory is served at `http://127.0.0.1:9080/` from the prebuilt `ui/dist`
directory.

Use `RUST_LOG=info` to enable runtime logs. Configuration files are local administrator
input, but they still run in a fresh Lua state with no standard libraries and bounded
source, memory, and instruction use.

## Develop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir ui test
pnpm --dir ui build
```

Development follows red-green-refactor. The initial configuration and runtime-planning
acceptance tests were run and observed failing before their implementations were added.

## Design

- [`docs/UPSTREAM_ANALYSIS.md`](docs/UPSTREAM_ANALYSIS.md) records the upstream findings.
- [`docs/PRODUCT_SPEC.md`](docs/PRODUCT_SPEC.md) defines goals and functional requirements.
- [`docs/CONFIG_SPEC.md`](docs/CONFIG_SPEC.md) defines the Lua and canonical config contracts.
- [`docs/IMPORT_SPEC.md`](docs/IMPORT_SPEC.md) defines native configuration compatibility.
- [`docs/API_UI_SPEC.md`](docs/API_UI_SPEC.md) defines control-plane and UI behavior.
- [`docs/ACME_SPEC.md`](docs/ACME_SPEC.md) defines certificate issuance and auto-renewal.
- [`docs/RTMP_SPEC.md`](docs/RTMP_SPEC.md) defines RTMP runtime and nginx-rtmp compatibility.
- [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md) distinguishes implemented, planned, and excluded capabilities.
- [`docs/TEST_STRATEGY.md`](docs/TEST_STRATEGY.md) defines test-first release gates.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) defines the narrow release sequence.
- [`docs/NAMING.md`](docs/NAMING.md) lists candidate final names.

## License

Apache License 2.0. Upstream projects retain their own licenses; no upstream source is
vendored in this repository.
