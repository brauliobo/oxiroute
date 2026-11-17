# OxiRoute

A Rust proxy and control plane for routing HTTP, TCP, UDP, and RTMP services from one typed configuration.
Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora), with a CLI and browser dashboard for
inspecting traffic, managing backends, and applying configuration changes.

**Version 6.5.2 · Apache-2.0**

Start with a local evaluation. Protocol support and migration coverage vary by feature; the
[compatibility matrix](docs/COMPATIBILITY.md) defines the current boundaries. Broad production readiness
and complete compatibility with other proxies have not been established.

## Start here

| Your task | Guide |
| --- | --- |
| Run your first reverse proxy | [Getting started](docs/user/GETTING_STARTED.md) |
| Operate listeners, backends, and reloads | [Operations](docs/user/OPERATING.md) · [Runbook](docs/user/RUNBOOK.md) |
| Inspect the dashboard | [Dashboard](docs/user/DASHBOARD.md) |
| Migrate an existing proxy configuration | [Migration](docs/user/MIGRATION.md) |
| Publish or record live streams | [RTMP](docs/user/RTMP.md) |
| Build an integration | [CLI](docs/reference/CLI.md) · [API](docs/reference/API.md) · [Configuration](docs/reference/CONFIGURATION.md) |
| Contribute code | [Developer guide](docs/developer/README.md) |
| Evaluate performance | [Benchmarks and evidence](docs/BENCHMARKS.md) |

## What you can do

| Area | Available behavior | Current boundary |
| --- | --- | --- |
| HTTP routing | Host/path/method routes; proxy, redirect, fixed and static responses; WebSocket upgrades; header rules, gzip, access logs | HTTP/1, HTTP/2, HTTP/3 and gRPC have different conformance limits |
| Backend pools | Round robin, weighted round robin, least connections, health checks, capacity admission, safe retries, passive health, lease-based server registration | Registration requires an opted-in existing pool; retry safety depends on request policy |
| Transport | Opaque TCP with half-close and timeouts; bounded UDP sessions; TLS termination and verified upstream TLS | UDP and supervised active-traffic drain have remaining gates |
| Forward proxy | Authenticated HTTP/1 forwarding and CONNECT, classic H2 CONNECT, bounded H3 forwarding, opt-in H1 CONNECT-UDP | Request forms and MASQUE support are limited; destination policy is explicit |
| Caching | Bounded memory/persistent GET/HEAD caches, revalidation, collapsed fills, authenticated purge | Eligibility and protocol limits apply; broader cache conformance remains |
| Configuration | KDL 2.0, restricted Lua, HOCON and UCI; typed validation, templates, native references, revision-checked saves | KDL is recommended; each adapter has explicit restrictions |
| Operations | CLI, dashboard, topology, Prometheus metrics, events, redacted audit/access logs, reload and rollback | Management is loopback-only with bearer authentication; active-traffic evidence varies by protocol |
| Media | RTMP publish/play, fanout, recording, HLS/DASH, VOD, relays and local controls | Recording supports legacy AVC/AAC; broader media compatibility remains partial |
| Migration | nginx, HAProxy, Apache, Squid and Varnish reports and previews; strict nginx-RTMP composition | Importers accept audited subsets and report blocking semantics |

Feature availability does not imply complete protocol support. Read the
[compatibility matrix](docs/COMPATIBILITY.md) before choosing a listener or replacing an existing service.
Managed ACME also remains partial: CA-staging and production active-traffic evidence are release gates.

## First local request

Follow [Getting started](docs/user/GETTING_STARTED.md) for a complete, copyable source-build tutorial.
It starts a local origin, validates the checked-in configuration, runs OxiRoute, and checks:

```sh
curl -i http://127.0.0.1:8080/healthz
```

Expected result: `200 OK` from the origin through OxiRoute. The full example also configures TCP, RTMP,
and the management dashboard; the guide explains the ports, token, verification, and cleanup.

Source builds use **Rust 1.97.1**, **Node.js 22**, **pnpm 11.3.0**, a C toolchain, and the platform build
prerequisites described in the guide. Linux provides the fullest monitoring and supervision support.

## Install a release

Each [GitHub release](https://github.com/brauliobo/oxiroute/releases) attaches Debian and RPM packages, binary
tarballs for `x86_64` and `aarch64`, and one `SHA256SUMS`. There is no hosted apt or yum repository yet; install the
file you downloaded, after `sha256sum --check --ignore-missing SHA256SUMS`:

```sh
sudo apt install ./oxiroute_<version>_amd64.deb                   # Debian 12/13, Ubuntu 22.04/24.04
sudo dnf install ./oxiroute-<version>-1.el9.x86_64.rpm            # AlmaLinux and Rocky Linux 9
sudo dnf install ./oxiroute-<version>-1.amzn2023.x86_64.rpm       # Amazon Linux 2023
```

The packages enable and start nothing; [Install a release package](docs/user/INSTALL.md) has the aarch64 names, the
tarball, the service opt-in, and upgrade, remove and purge behavior. For Arch Linux, see the
[Arch packaging guide](packaging/arch/README.md).

## How configuration changes work

```text
source → typed configuration → validation → prepared generation → active listeners
```

OxiRoute prepares a complete candidate before publishing it. A rejected candidate leaves the active
generation serving. Revision checks prevent a stale configuration editor from silently overwriting a
newer file. Existing traffic retains its generation while it drains; protocol-specific replacement
and drain limits are documented in the [operations contract](docs/OPERATIONS.md).

Start with [configuration examples](docs/reference/CONFIGURATION.md), then consult the
[full specification](docs/CONFIG_SPEC.md) for defaults, bounds, precedence and validation rules.

## Measured performance

A **maintainer-recorded historical integrated 6.5 evaluation**, rather than a measurement of the current
6.5.2 release, reported these medians for an HTTP/1.1 reverse proxy with a 1 KiB response:

| Measure | OxiRoute 6.5 integrated | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Requests/second | 159,762 | 180,958 | 156,066 |
| p99 latency, µs | 296 | 252 | 348 |
| Proxy CPU time/request, µs | 4.5 | 3.9 | 4.8 |

One worker per proxy, 32 keep-alive connections, loopback, CPU pinning, interleaved runs on a shared
host; three 8-second measured runs after 3-second warm-ups. This workload put OxiRoute below nginx
in throughput and near HAProxy. Other workloads differ.

These are historical summary records: the raw runs, exact binary identity and complete reproducibility
bundle are not published here. They cannot serve as an independently validated release baseline or
production sizing guarantee. [Benchmark evidence](docs/BENCHMARKS.md) includes ranges, additional
workloads, memory measurements, caveats and the requirements for a publishable comparison.

## Develop

Begin with the [developer guide](docs/developer/README.md) for setup, the crate map, architecture and
focused verification. Runtime changes need failure-path and interoperability evidence, along with updates
to the relevant user guide and capability status.

Typical Rust checks, from the repository root:

```sh
nice cargo +1.97.1 fmt --all --check
nice cargo +1.97.1 clippy --workspace --all-targets --locked --jobs 4 -- -D warnings
nice cargo +1.97.1 test --workspace --locked --jobs 4
```

The [testing guide](docs/developer/TESTING.md) covers UI, browser, contract, fuzz and protocol checks.
The [documentation hub](docs/README.md) connects tutorials, operational guides and exact references.

## License

[Apache License 2.0](LICENSE). Vendored dependencies retain their upstream licenses and document
provenance and local changes in their own directories.
