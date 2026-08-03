# OxiRoute

OxiRoute is a pre-alpha Rust proxy runtime and control plane for publishing HTTP, TCP, and RTMP
services from one typed configuration. It is built on [Cloudflare Pingora](https://github.com/cloudflare/pingora)
and is designed around explicit behavior, bounded inputs, observable runtime state, and safe
configuration changes.

**Current release line:** `0.3.0` (pre-alpha; `v0.2.3` is the latest repository tag)<br>
**Project status:** pre-alpha; read the [compatibility matrix](docs/COMPATIBILITY.md) before using it
for production traffic.<br>
**Website:** [brauliobo.github.io/oxiroute](https://brauliobo.github.io/oxiroute/)
**License:** Apache-2.0

## Start With Your Job

| If you want to... | Start here |
| --- | --- |
| Run a local reverse proxy | [User quickstart](docs/user/GETTING_STARTED.md) |
| Operate a running daemon | [Operations guide](docs/user/OPERATING.md) and [management CLI](docs/MANAGEMENT_CLI.md) |
| Understand the browser dashboard | [Dashboard guide](docs/user/DASHBOARD.md) |
| Migrate nginx, HAProxy, or Squid | [Migration guide](docs/user/MIGRATION.md) and [import specification](docs/IMPORT_SPEC.md) |
| Publish or record RTMP | [RTMP guide](docs/user/RTMP.md) and [RTMP specification](docs/RTMP_SPEC.md) |
| Integrate with the control plane | [API reference](docs/reference/API.md) and [API/UI specification](docs/API_UI_SPEC.md) |
| Change the code | [Developer guide](docs/developer/README.md) |
| Find a feature's exact boundary | [Compatibility matrix](docs/COMPATIBILITY.md) |

The [documentation hub](docs/README.md) explains the full hierarchy. The website is the quickest
visual overview; the repository documents are the detailed, versioned contracts.

## What Exists Today

OxiRoute is deliberately honest about the difference between a useful narrow slice and a product
goal. These are the current building blocks:

- **HTTP reverse proxy:** deterministic host/path/method routing, health-aware pools, round-robin and
  least-connections selection, active TCP/HTTP checks, bounded connect-failure retries, request and
  connection limits, WebSocket upgrades, downstream TLS, verified upstream TLS/SNI, and a tested
  HTTP/2/gRPC slice.
- **Explicit forward proxy:** a narrow HTTP/1 absolute-form and CONNECT path with authentication,
  resolved-address destination policy, header privacy, and bounded tunnels. It is partial, not a
  general Squid replacement.
- **Opaque TCP relay:** socket, DNS, and Unix upstreams, half-close handling, health-aware pools,
  backpressure, and connect/idle/lifetime limits.
- **Configuration control plane:** KDL 2.0 by default, plus restricted Lua, HOCON, and UCI source
  adapters; typed validation; deterministic previews; templates and strict native references; and
  revision-checked configuration writes. KDL is the canonical current format; restricted Lua is a
  supported compatibility adapter, not the canonical format.
- **Runtime observability:** loopback management API, topology graph, monitoring snapshots, Prometheus
  metrics, readiness, bounded event polling, redacted HTTP JSONL access logs, HAProxy-oriented
  statistics, and a responsive Vue 3/Pug dashboard.
- **RTMP live path:** publish/play, simple and complex handshakes, bounded fanout, keyframe gating,
  stream inventory, legacy AVC/AAC FLV recording, named continuous/manual recorder policies, a
  fixed 8 MiB inbound assembled-message ceiling, static push relay, and exact-ID bearer-protected
  local controls.
- **Native migration:** bounded nginx, HAProxy, and Squid import/report/preview paths, with source
  provenance and blocking diagnostics rather than silent lossy conversion. Complete nginx roots
  can compose the strict nginx-RTMP subset into the canonical runtime through native references.
- **Supervision foundations:** platform-neutral replacement state machines and a staged Linux master,
  worker, and launcher path. The default public entry point remains direct `oxiroute serve` while the
  supervised production path is gated.

## Five-Minute Local Run

This path uses the checked-in example. It starts one local HTTP origin, builds the dashboard, and runs
the daemon with loopback-only management enabled.

### 1. Start an origin

In terminal A, start a server that answers the example's health check:

```sh
python3 - <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            body = b"ok\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_error(404)

    def log_message(self, format, *args):
        pass

HTTPServer(("127.0.0.1", 3000), Handler).serve_forever()
PY
```

### 2. Build the dashboard and start OxiRoute

In terminal B:

```sh
pnpm --dir ui install
pnpm --dir ui build
umask 077
openssl rand -hex 32 > /tmp/oxiroute-management.token
OXIROUTE_MANAGEMENT_TOKEN_FILE=/tmp/oxiroute-management.token \
  cargo run -p oxiroute -- serve oxiroute.example.kdl
```

The example exposes:

| Surface | Address | Purpose |
| --- | --- | --- |
| HTTP proxy | `127.0.0.1:8080` | Proxies requests to the healthy origin on port `3000` |
| TCP relay | `127.0.0.1:15432` | Relays opaque traffic to `127.0.0.1:5432` when a target exists |
| RTMP listener | `127.0.0.1:1935` | Accepts configured live publish/play sessions |
| Management and UI | `127.0.0.1:9080` | Serves the dashboard and loopback API |

### 3. Verify the request path

```sh
curl -i http://127.0.0.1:8080/healthz
TOKEN=$(tr -d '\r\n' < /tmp/oxiroute-management.token)
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/monitoring | jq .
```

Open `http://127.0.0.1:9080/` to view the Runtime Observatory. Recognized management/API routes
require the bearer token except exact `GET /ready` and `GET /metrics`; the browser retains that
token in page memory only.

## The Configuration Model

Every supported source becomes the same strict Rust-owned model before validation and runtime
preparation:

```text
source file -> bounded adapter -> typed config -> validation -> runtime plan -> generation -> listeners
```

KDL 2.0 is the recommended authoring format. File extensions select compatibility adapters:

| Source | Use it when |
| --- | --- |
| `.kdl`, `.kdl2`, or no extension | Writing a new configuration, using templates, or referencing native sources |
| `.lua` | Keeping a legacy restricted-Lua configuration |
| `.hocon` or `.conf` | Integrating a declarative HOCON source |
| `.uci` | Integrating an OpenWrt-style source |

The canonical example is [`oxiroute.example.kdl`](oxiroute.example.kdl). The [configuration
reference](docs/reference/CONFIGURATION.md) explains the smallest useful shapes; the full
[configuration specification](docs/CONFIG_SPEC.md) defines bounds, defaults, routing precedence,
TLS, health checks, recording, and security rules.

A configuration change is not an in-place mutation. OxiRoute loads, validates, prepares, and then
publishes a complete generation. A failed candidate leaves the active generation serving traffic.
Management saves use a disk revision precondition, so a stale editor cannot silently overwrite a
newer file.

## Feature Boundaries

Use these labels consistently in issues, docs, and deployment decisions:

| Label | Meaning |
| --- | --- |
| `stable` | The behavior is part of the current narrow release contract with implementation and required evidence. |
| `partial` | A narrow path is present, but the compatibility or production gate is incomplete. |
| `foundation` | Tested component code exists, but it is not an active daemon capability. |
| `planned` | A product goal or roadmap item; do not configure it in the current daemon. |
| `research` | An evaluated possibility requiring a product or design decision. |
| `not-planned` | Deliberately excluded from the current product plan. |
| `out-of-scope` | It belongs to the kernel, a separate privileged helper, or another product boundary. |

The important current exclusions are:

- No HTTP/3 daemon listener, UDP relay, or complete forward-proxy HTTP/2/HTTP/3 runtime. The
  forward HTTP/2/HTTP/3 crates are foundations, not active listener capabilities.
- No active cache request path; the cache crate is a tested foundation only.
- No managed ACME issuance, certificate upload API, or certificate management UI. External Certbot
  lineage reconciliation is implemented.
- No complete nginx, HAProxy, Squid, Varnish, or nginx-RTMP compatibility. Importers finalize only
  audited, representable subsets and fail closed for blocking semantics; strict nginx-RTMP results
  are integrated through complete-root import and native references where documented.
- No remote multi-user management mode. Recognized management/API routes require bearer
  authentication except exact `GET /ready` and `GET /metrics`; management binds are loopback-only.
- No firewall, NAT, transparent interception, source spoofing, or arbitrary packet forwarding.

See [COMPATIBILITY.md](docs/COMPATIBILITY.md) for the capability-by-capability matrix and
[ROADMAP.md](docs/ROADMAP.md) for future milestones. Roadmap entries are not current support.

## Repository Map

| Path | Responsibility |
| --- | --- |
| `crates/oxiroute-server` | CLI, daemon, listeners, generations, API, monitoring, topology, TLS, and runtime wiring |
| `crates/oxiroute-config` | Typed canonical model, defaults, validation, and restricted Lua |
| `crates/oxiroute-config-source` | KDL, HOCON, UCI, templates, native-reference resolution, and rendering |
| `crates/oxiroute-import` | nginx, HAProxy, Squid, Varnish, provenance, diagnostics, and lowering |
| `crates/oxiroute-forward-proxy` | Protocol-neutral target parsing, authentication, destination policy, and bounded tunnels |
| `crates/oxiroute-rtmp` | RTMP sessions, fanout, recorder store/workers, FLV, directives, and relays |
| `crates/oxiroute-cache` | Standalone RFC-aware cache core; not connected to the request path |
| `crates/oxiroute-supervision*` | Replacement protocol, Unix transport, master, worker, and launcher foundations |
| `ui` | Vue 3 dashboard with build-time Pug templates and contract/component tests |
| `website` | Static public documentation site deployed by GitHub Pages |
| `remotion` | Deterministic dashboard walkthrough compositions and rendered documentation GIFs |
| `docs` | User journeys, developer notes, and normative product/reference specifications |

## Develop

Prerequisites are Rust `1.87`, Node.js with pnpm `11.3.0`, and a Linux environment for the
Linux-specific supervision and `/proc` paths. The normal local gates are:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir ui test
pnpm --dir ui build
```

Read [docs/developer/README.md](docs/developer/README.md) before changing runtime behavior. Read
[docs/developer/TESTING.md](docs/developer/TESTING.md) before adding a feature; the project treats
failure-path, observable-state, and interoperability coverage as part of support claims.

## Documentation Map

- [Documentation hub](docs/README.md)
- [User guides](docs/user/README.md)
- [Developer guides](docs/developer/README.md)
- [Reference index](docs/reference/README.md)
- [Compatibility matrix](docs/COMPATIBILITY.md)
- [Release notes](docs/RELEASE_NOTES_0.3.0.md)

## License

Apache License 2.0. Upstream projects retain their own licenses. The vendored Pingora and RTMP
sources document provenance and local deltas in their `vendor/*/README.oxiroute.md` files.
