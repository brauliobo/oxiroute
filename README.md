# OxiRoute

OxiRoute is a pre-alpha Rust proxy and traffic-routing project built on
[Cloudflare Pingora](https://github.com/cloudflare/pingora). The long-term goal is one
auditable control plane for HTTP proxying, layer-4 relays, load balancing, configuration
imports, and operational visibility.

The current code is intentionally much smaller than that goal. It provides:

- A restricted Lua configuration loader with strict typed validation.
- Pingora-backed HTTP reverse proxy listeners with deterministic host/path/method routing,
  health-aware typed socket/DNS/Unix pools, static round-robin or least-connections balancing,
  active TCP/HTTP checks for non-Unix pools, bounded safe connect-failure retries, upstream I/O
  timeouts, nullable body limits, and nullable connection caps.
- Strict startup-snapshotted direct-file identities and continuously reconciled Certbot identities
  with exact/wildcard SNI selection, an explicit default identity, a TLS 1.2 or 1.3 minimum, and
  explicit HTTP/1.1/H2 ALPN, plus verified upstream TLS with SNI, optional custom CA bundles, and
  explicit HTTP/1.1/H2 policy.
  Upstream handshakes enforce TLS 1.2+ and modern AEAD ciphers through a documented pinned Pingora
  connector patch.
- Wire-tested downstream TLS over HTTP/1.1 and HTTP/2, H2 upstream negotiation, and gRPC response
  DATA and trailers on the supported TLS/H2 path.
- HTTP/1.1 WebSocket upgrade proxying with bidirectional frame integration coverage.
- Pingora-backed opaque TCP relays with socket, DNS, or Unix upstreams, health-aware round-robin or
  least-connections pools, nullable connection caps, and configurable connect, idle, and lifetime
  timeouts. Unix listeners and upstreams require a Unix platform.
- Bounded native-import foundations for nginx and HAProxy with ordered source loading, syntax and
  semantic reports, stable diagnostics, provenance, and conservative canonical lowering. nginx
  HTTP remains draft-only; a strict static HAProxy TCP subset, including exact Unix/DNS endpoints
  and `leastconn`, can finalize. The complete audited host candidates remain blocked.
- An exact nginx-rtmp registry for all 117 active directive keys plus a separate strict RTMP
  include/inheritance lowerer that can finalize an error-free listener/application/recording subset
  with provenance and terminal occurrence accounting.
- RTMP live publish/play with simple/complex handshakes, explicit application policy, bounded
  per-viewer fanout, duplicate-publisher rejection, late-join keyframe gating, and native wire tests.
- Canonical continuous/manual RTMP recorder policies wired through publisher media dispatch to
  bounded nonblocking disk workers, legacy AVC/AAC FLV muxing, safe relative naming, keyframe-aligned
  rotation, descriptor-pinned storage, atomic publication, process-scoped quotas, and bounded
  shutdown/reaping.
- A loopback-only Pingora management API for runtime and pool-health monitoring, RTMP stream
  visibility, exact-ID recording controls, and authenticated canonical configuration read,
  validation, preview, and revision-checked durable writes.
- A responsive Vue/Pug runtime observatory for host/process load, listener traffic, upstream
  health, and live RTMP state, plus a typed canonical configuration workspace with server-side
  validation, Lua preview, conflict handling, and explicit save review.
- Acceptance tests for configuration isolation, runtime planning, and independent TLS/H2 wire
  interoperability.

It does not yet provide forward proxying, `CONNECT`, h2c, HTTP/3, UDP, caching, weighted load
balancing, passive failure ejection, hot reload, direct certificate-file reload/managed ACME
activation, TLS client authentication, daemon-integrated configuration imports, or complete
nginx-rtmp compatibility. Recording does not support enhanced AVC, HEVC, or AV1 output, and storage
quotas are not coordinated across daemon processes.
It is not a firewall, NAT implementation, or drop-in replacement for Squid, nginx,
HAProxy, or Apache httpd.

## Run

Start an upstream HTTP server on `127.0.0.1:3000` that returns `200` for `GET /healthz`, then run:

```sh
pnpm --dir ui install
pnpm --dir ui build
umask 077
openssl rand -hex 32 > /tmp/oxiroute-management.token
OXIROUTE_MANAGEMENT_TOKEN_FILE=/tmp/oxiroute-management.token \
  cargo run -p oxiroute-server -- oxiroute.example.lua
```

The example exposes the HTTP upstream on `127.0.0.1:8080`, defines a TCP relay from
`127.0.0.1:15432` to `127.0.0.1:5432`, and accepts RTMP publishers on
`rtmp://127.0.0.1:1935/<application>/<stream>`. The HTTP pool actively checks `/healthz`; requests
fail with `503` until its first successful probe. Runtime monitoring and RTMP status are available at
`http://127.0.0.1:9080/api/v1/monitoring` and `http://127.0.0.1:9080/api/v1/rtmp/streams`.
The Vue/Pug runtime observatory is served at `http://127.0.0.1:9080/` from the prebuilt `ui/dist`
directory. Its configuration workspace asks for the token from the file and keeps it only in page
memory. The daemon requires `OXIROUTE_MANAGEMENT_TOKEN_FILE` whenever `management` is configured;
see [`docs/API_UI_SPEC.md`](docs/API_UI_SPEC.md) for the token-file and configuration API contract.
The distributed example intentionally remains plaintext and needs no certificate files. See the
canonical TLS example in [`docs/CONFIG_SPEC.md`](docs/CONFIG_SPEC.md) for HTTPS listener and
upstream configuration.

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

Apache License 2.0. Upstream projects retain their own licenses. The patched `pingora-core`
source is vendored under its upstream Apache-2.0 license; its provenance and delta are documented
in `vendor/pingora-core/README.oxiroute.md`.
