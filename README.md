# OxiRoute

A Rust proxy and control plane for routing HTTP, TCP, UDP, and RTMP services from one typed configuration.
Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora), with a CLI and browser dashboard for
inspecting traffic, managing backends, and applying configuration changes.

**Version 7.0.0 · Apache-2.0**

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

A **maintainer-recorded evaluation of the 7.0.0 release** (measured 2026-11-17 on the published profile-guided
x86_64 binary) reported these medians for an HTTP/1.1 reverse proxy with a 1 KiB response:

| Measure | OxiRoute 7.0 | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Requests/second | 178,396 | 134,061 | 114,860 |
| p99 latency, µs | 254 | 350 | 390 |
| Proxy CPU time/request, µs | 5.5 | 7.4 | 8.7 |

One worker per proxy, 32 keep-alive connections, loopback, CPU pinning, interleaved runs on a shared
host; two 4-second measured runs after 2-second warm-ups. This workload put OxiRoute above nginx and HAProxy in
throughput. Other workloads differ.
The [7.0 report](benchmarks/reports/7.0/README.md) adds 64 KiB, 1 MiB, 1,024-connection, TLS, HTTP/2, cache,
compression, HTTP/3, tokio-plane, io_uring, two-worker and memory results.

The matrix below lists every measured scope: the shipped PGO x86_64 7.0.0 binary on a 16-core desktop CPU, pinned
CPUs, loopback, 4-second runs, measured 2026-11-17 ([7.0 report](benchmarks/reports/7.0/README.md); ranges, PSI and
run counts in [docs/BENCHMARKS.md](docs/BENCHMARKS.md)). Cells are requests/second / p99 µs unless a row says
otherwise; `n/a` means not measured, `no clean run` means every attempt was discarded, and `(indicative)` is one clean
run.

HTTP/1.1, 1 KiB response and 32 connections unless stated:

| Test | OxiRoute default (epoll) | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| Keep-alive, 32 connections | 178,396 / 254 | 145,374 / 260 | 168,510 / 238 | 134,061 / 350 | 114,860 / 390 |
| Keep-alive, 256 connections | 171,930 / 4,415 (indicative) | 143,662 / 1,989 | 162,403 / 2,218 (indicative) | no clean run | 106,794 / 3,654 |
| Keep-alive, 1,024 connections | 179,035 / 6,228 (indicative) | 135,597 / 158,184 (indicative) | 149,092 / 19,291 (indicative) | 92,548 / 18,658 | 79,344 / 15,580 |
| 64 KiB response | 91,602 / 620 | 53,337 / 884 (indicative) | 59,642 / 746 | 34,961 / 1,292 | 38,988 / 1,006 |
| 1 MiB response | 5,300 / 12,220 | 4,201 / 19,482 | 3,230 / 13,668 | 2,056 / 22,570 | 2,983 / 15,039 (indicative) |
| New connection per request | 91,432 / 752 | 80,070 / 586 | 92,178 / 516 | no clean run | 62,840 / 741 (indicative) |
| POST 1 KiB | 180,102 / 314 (indicative) | no clean run | 168,522 / 245 (indicative) | no clean run | 114,382 / 398 |
| POST 64 KiB | 47,063 / 787 | 40,960 / 1,435 (indicative) | 39,531 / 1,790 (indicative) | no clean run | 32,711 / 1,178 (indicative) |
| Two workers, 64 connections | 177,640 / 528 | n/a | n/a | no clean run | 218,280 / 398 |
| Two workers, 256 connections | 183,176 / 1,590 | n/a | n/a | no clean run | 206,878 / 1,932 |
| Two workers, 256 connections, `aged_per_worker` (opt-in) | 340,345 / 998 (indicative) | n/a | n/a | n/a | n/a |

TLS, 32 connections, ECDSA P-256, rustls (the default HTTPS listener uses the tokio path; `io_uring` does not serve TLS):

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 1 KiB | 128,113 / 416 | 132,840 / 384 | n/a | 96,906 / 442 | 84,794 / 636 |
| TLS 1.3 keep-alive, 1 KiB, `http_data_plane = "epoll"` set explicitly | 154,726 / 246 | n/a | n/a | n/a | n/a |
| TLS 1.3 full handshake | 16,092 / 2,248 | n/a | n/a | 5,131 / 6,797 | 4,674 / 9,654 |
| TLS 1.2 ECDHE full handshake | 16,770 / 4,487 | n/a | n/a | 5,404 / 7,258 | 4,864 / 7,394 |
| TLS 1.3 resumed (ticket) | 17,386 / 3,095 (indicative) | n/a | n/a | 5,226 / 11,323 | 4,592 / 8,198 |
| TLS 1.2 resumed (ticket) | 57,527 / 1,022 | n/a | n/a | 11,902 / 3,646 | 8,871 / 4,089 |

HTTP/2 over TLS, 8 connections of 4 streams (upstream HTTP/2: instruction-count harness, 16 connections, OxiRoute only):

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | 132,781 / 374 | n/a | n/a | 105,442 / 350 (indicative) | 85,054 / 546 |
| 64 KiB | 27,914 / 2,103 | n/a | n/a | 17,426 / 4,983 (indicative) | 20,706 / 1,878 |
| POST 1 KiB | 113,696 / 424 | n/a | n/a | 72,002 / 546 | 71,801 / 625 |
| Upstream HTTP/2, 1 KiB | 41,725 / 508 | n/a | n/a | n/a | n/a |
| Upstream HTTP/2, POST 1 KiB | 31,488 / 791 | n/a | n/a | n/a | n/a |

Cache hits and compression, 32 connections:

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB cache hit | 270,224 / 146 | 267,722 / 145 | 267,454 / 142 | 129,656 / 274 (indicative) | 186,358 / 248 |
| 64 KiB cache hit | 106,662 / 624 | 113,590 / 499 | 108,739 / 565 (indicative) | 98,567 / 684 (indicative) | n/a |
| 64 KiB gzip level 1, stored coding, hit (nginx compresses per request) | 129,376 / 494 | n/a | n/a | 44,746 / 910 | n/a |
| 64 KiB gzip level 1, no cache | 33,202 / 1,510 | n/a | n/a | 35,131 / 1,088 | 24,783 / 1,894 |

HTTP/3, one worker, loopback (instruction-count harness, median of 3; the nginx proxy figure is from the comparison
harness, one clean run, not a like-for-like ratio). The last row is a 10 MiB download at 40 ms round trip and 0.5%
packet loss, seconds, median / p99:

| Test | OxiRoute `bbr` (default) | OxiRoute `cubic` | nginx HTTP/3 | nginx HTTP/2 over TCP |
| --- | ---: | ---: | ---: | ---: |
| Proxy, 8 connections of 4 streams, 1 KiB | 72,178 / 632 | n/a | 66,358 / n/a | n/a |
| Direct 1 KiB response | 198,369 / 229 | n/a | 126,442 / n/a | n/a |
| Proxy POST 1 KiB | 62,792 / 729 | n/a | n/a | n/a |
| New QUIC connection per request, direct | 5,256 / 2,273 | n/a | n/a | n/a |
| 10 MiB download, lossy path, s | 0.629 / 0.668 | 7.512 / 7.909 | 27.337 / 29.692 | 0.683 / 8.412 |

Static files and other protocols (the first two rows 32 connections; the others are OxiRoute-only
instruction-count harness lanes, 16 connections):

| Test | OxiRoute default | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Static file 1 MiB | 3,452 / 18,790 | 1,661 / 27,098 | n/a |
| Static file 1 MiB over TLS 1.3 (OxiRoute kernel TLS, nginx user-space TLS) | 3,157 / 11,895 (indicative) | 681 / 52,358 | n/a |
| Static file 1 KiB | 357,327 / 61 | no clean run | n/a |
| WebSocket echo, 1 KiB message | 174,142 / 114 | n/a | n/a |
| TCP relay echo, 1 KiB | 203,193 / 99 | n/a | n/a |
| TCP relay echo, 64 KiB | 77,778 / 366 | n/a | n/a |
| Pool `least_connections`, 8 servers | 175,280 / 125 | n/a | n/a |
| Pool `weighted_round_robin`, 8 servers | 177,533 / 123 | n/a | n/a |
| HTTP/1.1 keep-alive, 1 connection | 45,689 / 31 | n/a | n/a |
| HTTP/1.1, 2 KiB request headers | 142,306 / 157 | n/a | n/a |
| Reverse proxy with 7 production features on | 155,098 / n/a | n/a | n/a |

Memory, one worker (PSS in MB; incremental resident memory per idle connection in bytes, 5,000 connections, HTTP/2
1,000):

| Test | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| PSS idle, MB | 13.2 | n/a | n/a |
| PSS, 32 connections held / drained, MB | 13.8 / 13.9 | n/a | n/a |
| PSS, 1,024 connections held / drained, MB | 15.6 / 15.7 | n/a | n/a |
| Per idle HTTP/1.1 keep-alive connection, bytes | 1,749 | 9,989 | 11,728 |
| Per idle HTTP/1.1 connection over TLS, bytes | 16,749 | 34,784 | 35,762 |
| Per idle WebSocket connection, bytes | 12,526 | 18,364 | 5,693 |
| Per idle WebSocket connection over TLS, bytes | 21,634 | 59,832 | 31,188 |
| Per idle HTTP/2 over TLS connection, bytes | 29,864 | 48,792 | 47,074 |

Not measured:

- HTTP/1.1 at 2,048 connections, the 1-connection latency lane against nginx and HAProxy, and the four-worker,
  512 to 2,048 connection and dynamic-scaling matrices (not run).
- Access log, request id, header rewrite, 1,000-route, 4-origin, redirect and basic authentication lanes (not run).
- RSA-2048 and client certificate handshakes, TLS 1.2 keep-alive and resumption by session id (not run).
- Brotli and zstd cache hits (the load generator verifies only gzip bodies).
- Tokio and `io_uring` columns of the TLS, HTTP/2, compression and handshake lanes (not run; `io_uring` does not
  serve TLS).
- HTTP/3 comparison against nginx in the harness (no clean OxiRoute run: the QUIC runtime uses more threads than the
  scheduler-trace validation accepts) and the `cubic` request-rate lane.
- Reload memory (no checked-in lane), the BBR fairness matrix (tracked results are in the repository), forward proxy
  and RTMP (no harness lane), and an arm64 binary.

These are summary records: the raw runs, exact binary identity and complete reproducibility
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
