# OxiRoute

A Rust proxy and control plane for routing HTTP, TCP, UDP, and RTMP services from one typed configuration.
Built on [Cloudflare Pingora](https://github.com/cloudflare/pingora), with a CLI and browser dashboard for
inspecting traffic, managing backends, and applying configuration changes.

**Version 7.1.0 · Apache-2.0**

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
CPUs, loopback, 4-second runs after 2-second warm-ups, measured 2026-11-17 and 2026-11-18 ([7.0
report](benchmarks/reports/7.0/README.md); ranges, PSI and run counts in [docs/BENCHMARKS.md](docs/BENCHMARKS.md)).
Every number is the median of at least two clean runs of the same method. Cells are requests/second / p99 µs unless a
row says otherwise. `n/a` means the lane does not apply to that column; the row label or a note gives the reason.

HTTP/1.1, 1 KiB response and 32 connections unless stated:

| Test | OxiRoute default (epoll) | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| Keep-alive, 32 connections | 178,396 / 254 | 145,374 / 260 | 168,510 / 238 | 134,061 / 350 | 114,860 / 390 |
| Keep-alive, 256 connections | 180,487 / 1,771 | 143,662 / 1,989 | 165,160 / 2,432 | 122,657 / 2,460 | 106,794 / 3,654 |
| Keep-alive, 1,024 connections | 166,806 / 9,730 | 130,310 / 159,402 | 147,642 / 10,170 | 92,548 / 18,658 | 79,344 / 15,580 |
| Keep-alive, 2,048 connections | 159,112 / 14,576 | 126,575 / 349,352 | 133,476 / 42,583 | 71,364 / 48,150 | 61,604 / 50,182 |
| 64 KiB response | 91,602 / 620 | 51,338 / 1,022 | 59,642 / 746 | 34,961 / 1,292 | 38,988 / 1,006 |
| 1 MiB response | 5,300 / 12,220 | 4,201 / 19,482 | 3,230 / 13,668 | 2,056 / 22,570 | 2,692 / 17,556 |
| New connection per request | 91,432 / 752 | 80,070 / 586 | 92,178 / 516 | 69,558 / 694 | 60,109 / 748 |
| POST 1 KiB | 171,858 / 228 | 133,159 / 293 | 164,172 / 262 | 112,460 / 370 | 114,382 / 398 |
| POST 64 KiB | 47,063 / 787 | 37,802 / 1,131 | 38,800 / 1,278 | 26,976 / 2,124 | 30,322 / 1,287 |
| Two workers, 64 connections | 177,640 / 528 | 249,088 / 426 | 303,192 / 389 | 249,555 / 364 | 218,280 / 398 |
| Two workers, 256 connections | 183,176 / 1,590 | 267,236 / 1,610 | 291,636 / 2,644 | 248,264 / 1,390 | 206,878 / 1,932 |
| Two workers, 64 connections, `aged_per_worker` (opt-in; epoll plane only) | 326,586 / 302 | n/a | n/a | n/a | n/a |
| Two workers, 256 connections, `aged_per_worker` (opt-in; epoll plane only) | 283,586 / 2,878 | n/a | n/a | n/a | n/a |

The default epoll plane at two workers and 64 or 256 connections is bimodal. In a fresh process the first load run
spread connections over both workers (273,000 to 354,000 requests/second, both worker threads busy); later runs from
the same process kept all connections on one worker thread (about 181,000 requests/second, one thread busy for the whole
run). The two-worker rows are the harness medians, which land on the one-worker mode, so they trail nginx, tokio,
`io_uring` and `aged_per_worker`. The cause in the connection distribution is not isolated. The tokio plane at two
workers fails about one request in a million with `UnexpectedEof` (a keep-alive connection closed mid-request); the
harness discards runs with request errors, so those cells needed up to six attempts for two clean runs.

Worker scaling, epoll plane, 1 KiB response (`aged_per_worker` is an opt-in `connection_scheduling` mode):

| Test | OxiRoute default (epoll) | OxiRoute `aged_per_worker` | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: |
| One worker, 512 connections | 183,648 / 4,328 | 181,716 / 3,480 | 125,426 / 6,054 | 103,606 / 7,041 |
| One worker, 2,048 connections | 152,380 / 22,921 | 152,884 / 24,514 | 72,082 / 31,860 | 64,124 / 35,394 |
| One worker, 512 connections, 1 in 500 origin responses delayed 200 ms | 174,850 / 2,751 | 173,242 / 2,824 | 117,462 / 9,102 | 102,450 / 5,514 |
| Two workers, 512 connections | 260,457 / 3,352 | 336,584 / 2,250 | 252,868 / 2,954 | 206,559 / 3,158 |
| Two workers, 1,024 connections | 285,210 / 7,664 | 310,097 / 9,716 | 206,178 / 5,618 | 160,014 / 7,530 |
| Two workers, 2,048 connections | 308,658 / 20,046 | 270,051 / 12,636 | 137,250 / 23,322 | 118,959 / 32,938 |
| Two workers, 512 connections, 1 in 500 origin responses delayed 200 ms | 212,216 / 3,624 | 314,895 / 2,468 | 232,924 / 4,132 | 200,084 / 2,541 |
| Four workers, 64 connections | 262,670 / 398 | 331,513 / 375 | 348,248 / 326 | 352,972 / 324 |
| Four workers, 256 connections | 266,735 / 1,598 | 344,030 / 1,218 | 352,074 / 1,583 | 357,568 / 2,146 |
| Four workers, 512 connections | 272,122 / 3,050 | 332,712 / 6,988 | 345,814 / 6,548 | 338,920 / 6,560 |
| Four workers, 1,024 connections | 264,854 / 10,338 | 295,522 / 20,348 | 300,136 / 18,709 | 299,608 / 18,706 |
| Four workers, 2,048 connections | 271,604 / 26,052 | 282,846 / 45,376 | 266,907 / 45,483 | 229,849 / 39,006 |
| Four workers, 512 connections, 1 in 500 origin responses delayed 200 ms | 239,054 / 3,776 | 318,366 / 4,632 | 324,852 / 4,290 | 303,745 / 4,696 |

HTTPS and HTTP/2 over TLS, one worker, many connections (the TLS listener runs the tokio path; `aged_per_worker` is the
opt-in scheduling mode):

| Test | OxiRoute default | OxiRoute `aged_per_worker` | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 512 connections | 115,171 / 68,332 | 109,966 / 14,490 | 86,158 / 13,220 | 77,212 / 7,618 |
| TLS 1.3 keep-alive, 1,024 connections | 109,691 / 167,276 | 82,268 / 37,704 | 62,962 / 18,842 | 55,584 / 22,982 |
| TLS 1.3 keep-alive, 2,048 connections | 111,988 / 358,928 | 81,552 / 76,550 | 43,720 / 82,095 | 43,892 / 50,394 |
| HTTP/2 over TLS, 128 connections of 4 streams | 120,869 / 38,278 | 120,216 / 10,116 | 92,706 / 7,032 | 80,662 / 7,165 |
| HTTP/2 over TLS, 256 connections of 4 streams | 117,671 / 97,820 | 99,607 / 23,918 | 73,317 / 21,813 | 65,752 / 17,010 |
| HTTP/2 over TLS, 512 connections of 4 streams | 115,432 / 213,066 | 79,068 / 58,680 | 54,493 / 54,776 | 56,720 / 39,148 |

TLS, 32 connections, ECDSA P-256, rustls. The `io_uring` column is `n/a` because that plane does not serve TLS: with
`http_data_plane = "io_uring"` the TLS listener runs the tokio path, so there is no separate `io_uring` TLS result.

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 1 KiB | 128,113 / 416 | 132,840 / 384 | n/a | 96,906 / 442 | 84,794 / 636 |
| TLS 1.3 keep-alive, 1 KiB, `http_data_plane = "epoll"` set explicitly (OxiRoute option) | 154,726 / 246 | n/a | n/a | n/a | n/a |
| TLS 1.2 keep-alive, 1 KiB | 120,756 / 338 | 118,990 / 388 | n/a | 93,872 / 487 | 85,790 / 462 |
| TLS 1.3 full handshake | 16,092 / 2,248 | 15,596 / 2,407 | n/a | 5,131 / 6,797 | 4,674 / 9,654 |
| TLS 1.2 ECDHE full handshake | 16,770 / 4,487 | 17,063 / 2,413 | n/a | 5,404 / 7,258 | 4,864 / 7,394 |
| TLS 1.3 resumed (ticket) | 17,012 / 2,448 | 17,125 / 2,442 | n/a | 5,226 / 11,323 | 4,592 / 8,198 |
| TLS 1.2 resumed (ticket) | 57,527 / 1,022 | 53,758 / 1,129 | n/a | 11,902 / 3,646 | 8,871 / 4,089 |
| TLS 1.2 resumed (session id) | 57,089 / 1,050 | 56,312 / 1,096 | n/a | 11,937 / 3,271 | 9,076 / 4,030 |
| TLS 1.3 full handshake, RSA-2048 certificate | 1,871 / 28,930 | 1,938 / 17,798 | n/a | 1,536 / 22,445 | 1,512 / 22,476 |
| TLS 1.3 full handshake with a client certificate | 5,273 / 8,090 | 5,288 / 6,561 | n/a | 2,746 / 12,628 | 2,613 / 13,224 |

HTTP/2 over TLS, 8 connections of 4 streams. The upstream HTTP/2 rows use an HTTP/1.1 front end with 32 connections and an
HTTP/2 upstream. The `io_uring` column is `n/a` on the front-end rows for the same reason as TLS:

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | 132,781 / 374 | 128,492 / 388 | n/a | 94,742 / 406 | 85,054 / 546 |
| 64 KiB | 27,914 / 2,103 | 28,144 / 1,820 | n/a | 18,159 / 2,192 | 20,706 / 1,878 |
| POST 1 KiB | 113,696 / 424 | 115,244 / 420 | n/a | 72,002 / 546 | 71,801 / 625 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end, 32 connections) | 40,125 / 1,226 | 39,620 / 1,228 | 39,476 / 1,322 | 87,964 / 404 | 87,490 / 582 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end, 32 connections) | 32,580 / 1,314 | 31,590 / 1,562 | 32,220 / 1,332 | 78,782 / 582 | 89,446 / 588 |

With an HTTP/2 upstream OxiRoute serves about half the requests/second of nginx and HAProxy; with an HTTP/1.1 upstream it
is ahead of both.

Cache hits and compression, 32 connections. nginx and HAProxy compress per request and have no stored-coding cache;
neither has a brotli or zstd encoder in the measured builds, so those rows are `n/a`:

| Test | OxiRoute default | OxiRoute tokio | OxiRoute io_uring | nginx | HAProxy |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB cache hit | 270,224 / 146 | 267,722 / 145 | 267,454 / 142 | 131,942 / 265 | 186,358 / 248 |
| 64 KiB cache hit | 106,662 / 624 | 113,590 / 499 | 110,002 / 630 | 98,718 / 701 | 57,800 / 716 |
| 64 KiB gzip level 1, stored coding, hit (nginx and HAProxy compress per request) | 129,376 / 494 | 135,674 / 406 | 137,866 / 404 | 44,746 / 910 | 27,816 / 1,316 |
| 64 KiB zstd level 1, stored coding, hit | 172,212 / 442 | 146,226 / 501 | 195,873 / 316 | n/a | n/a |
| 64 KiB brotli level 1, stored coding, hit | 111,320 / 578 | 124,931 / 491 | 128,348 / 484 | n/a | n/a |
| 64 KiB gzip level 1, no cache | 33,202 / 1,510 | 32,861 / 1,691 | 32,459 / 1,274 | 35,131 / 1,088 | 24,783 / 1,894 |

HTTP/3, one worker, loopback, comparison harness. The `cubic` column is the same OxiRoute listener with
`http3_congestion_control = "cubic"`. The last row is a 10 MiB download at 40 ms round trip and 0.5% packet loss from
the impairment harness, seconds, median / p99:

| Test | OxiRoute `bbr` (default) | OxiRoute `cubic` | nginx HTTP/3 | nginx HTTP/2 over TCP |
| --- | ---: | ---: | ---: | ---: |
| Proxy, 8 connections of 4 streams, 1 KiB | 70,169 / 796 | 70,101 / 743 | 66,773 / 686 | 94,742 / 406 |
| Direct 1 KiB response | 189,926 / 314 | 193,052 / 300 | 125,342 / 440 | 176,600 / 267 |
| Proxy POST 1 KiB | 64,250 / 774 | 64,849 / 687 | 60,725 / 896 | 72,002 / 546 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | 5,250 / 2,501 | 5,140 / 3,388 | 3,376 / 2,729 | 5,057 / 1,886 |
| 10 MiB download, lossy path, s | 0.629 / 0.668 | 7.512 / 7.909 | 27.337 / 29.692 | 0.683 / 8.412 |

Static files, other protocols and features, 32 connections unless stated. HAProxy does not serve files from disk, so the
static rows are `n/a` for it. The last rows are OxiRoute-only instruction-count harness lanes, 16 connections; the
comparators were not configured as forward proxies (see Not measured):

| Test | OxiRoute default | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Static file 1 MiB | 1,699 / 21,805 | 1,668 / 20,546 | n/a |
| Static file 1 MiB over TLS 1.3 (OxiRoute kernel TLS, nginx user-space TLS) | 1,394 / 25,537 | 694 / 48,673 | n/a |
| Static file 1 KiB | 343,474 / 189 | 78,452 / 449 | n/a |
| WebSocket echo, 1 KiB message, 16 connections | 174,002 / 138 | 225,079 / 98 | 170,088 / 139 |
| TCP relay echo, 1 KiB, 16 connections | 202,172 / 144 | 208,781 / 96 | 183,838 / 130 |
| TCP relay echo, 64 KiB, 16 connections | 71,078 / 430 | 32,340 / 717 | 28,798 / 903 |
| Pool `least_connections`, 8 servers | 177,662 / 264 | 130,749 / 357 | 113,962 / 478 |
| Pool `weighted_round_robin`, 8 servers | 183,626 / 208 | 131,884 / 350 | 113,846 / 442 |
| HTTP/1.1 keep-alive, 1 connection | 39,430 / 36 | 35,394 / 42 | 35,632 / 42 |
| HTTP/1.1, 2 KiB request headers | 141,700 / 334 | 92,754 / 438 | 98,984 / 473 |
| Access log on | 151,706 / 302 | 81,698 / 474 | 69,423 / 569 |
| Request id on | 167,191 / 282 | 111,407 / 338 | 94,566 / 495 |
| Request and response header rules | 168,756 / 324 | 128,982 / 436 | 111,549 / 441 |
| 1,000 exact routes plus a catch-all | 181,623 / 298 | 133,808 / 338 | 116,588 / 386 |
| 4 origin servers, round robin | 184,829 / 238 | 131,421 / 430 | 114,340 / 467 |
| Literal 301 redirect, no upstream | 383,152 / 165 | 301,240 / 124 | 259,642 / 237 |
| Basic authentication, bcrypt cost 10, same credentials every request (OxiRoute does not run bcrypt per request) | 90,646 / 398 | 26 / 1,223,695 | 26 / 2,217,885 |
| Reverse proxy with 7 production features on (instruction-count harness, 16 connections; no nginx or HAProxy equivalent of breaker, outlier ejection and capacity admission) | 159,847 / 134 | n/a | n/a |
| Forward proxy, HTTP/1.1 absolute-URI request, 1 KiB (OxiRoute only, instruction-count harness, 16 connections; no comparator configuration) | 23,459 / 14,940 | n/a | n/a |
| Forward proxy, CONNECT tunnel echo, 1 KiB (OxiRoute only, instruction-count harness, 16 connections; nginx answers CONNECT with 405) | 175,043 / 111 | n/a | n/a |
| Forward proxy, new CONNECT tunnel per request (OxiRoute only, instruction-count harness, 16 connections; no comparator configuration) | 24,879 / 853 | n/a | n/a |

Memory, one worker (PSS in MB from the comparison harness; incremental resident memory per idle connection in bytes, 5,000
connections, HTTP/2 1,000). Reloads are `generation reload` for OxiRoute, `SIGHUP` for nginx and `SIGUSR2` with
master-worker mode for HAProxy, each followed by settling to one generation:

| Test | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| PSS idle, MB | 9.9 | 10.6 | 30.9 |
| PSS, 32 connections held / drained, MB | 10.5 / 10.5 | 11.0 / 11.0 | 31.4 / 31.4 |
| PSS, 1,024 connections held / drained, MB | 12.2 / 12.2 | 19.9 / 19.9 | 40.1 / 40.1 |
| PSS idle, 1,001-route configuration, MB | 19.8 | 17.7 | 36.6 |
| PSS after 1 / 10 / 50 configuration reloads, 1,001-route configuration, MB | 30.8 / 39.3 / 41.6 | 25.3 / 25.5 / 26.2 | 34.9 / 34.9 / 36.3 |
| Per idle HTTP/1.1 keep-alive connection, bytes | 1,749 | 9,989 | 11,728 |
| Per idle HTTP/1.1 connection over TLS, bytes | 16,749 | 34,784 | 35,762 |
| Per idle WebSocket connection, bytes | 12,526 | 18,364 | 5,693 |
| Per idle WebSocket connection over TLS, bytes | 21,634 | 59,832 | 31,188 |
| Per idle HTTP/2 over TLS connection, bytes | 29,864 | 48,792 | 47,074 |

Not measured:

- RTMP: the harness has no RTMP load client and the nginx build has no RTMP module.
- An arm64 binary: no arm64 host.
- The BBR fairness matrix is not part of this comparison; its tracked results are in `benchmarks/r2-data-path/fairness/results`.
- Forward proxy against nginx and HAProxy: nginx answers `CONNECT` with 405, and no forward-proxy configuration was
  written for either.

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
