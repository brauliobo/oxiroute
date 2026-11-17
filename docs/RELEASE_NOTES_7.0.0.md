# OxiRoute 7.0.0

Prepared 2026-11-16 for the `v7.0.0` tag.

OxiRoute 7.0.0 changes the data path that serves plain HTTP/1.1 proxy traffic, the default TLS library and the default
HTTP/3 congestion controller. It is a major release because four defaults change (see
[Defaults that changed](#defaults-that-changed)); each one has a one-line setting that restores the 6.5 behaviour. The
release retains conservative compatibility claims: the [compatibility matrix](COMPATIBILITY.md) is authoritative.

The performance figures below come from the [7.0 report](../benchmarks/reports/7.0/README.md), which states the
host class, CPU pinning, tools, run counts and noise flags of every table. The headline HTTP/1.1, cache, TLS handshake
and HTTP/3 figures were re-measured on 2026-11-17 on the final tree without profile guidance; figures marked as earlier
come from the integration build measured on 2026-11-16 and 2026-11-17. All ran on a shared 16-core desktop CPU with
one worker per proxy and loopback traffic; the profile-guided release binaries were not measured. They are not
capacity guarantees.

## Headlines

- **A per-core epoll data plane serves plain HTTP/1.1 proxy routes.** One thread per `listener_workers`, with the same
  routing, limits, logs and metrics as before. HTTP/1.1 keep-alive with a 1 KiB response, 32 connections, one worker:
  169,706 requests per second (2/2 clean runs) against 128,043 on the tokio plane and 134,205 for nginx (one clean run,
  indicative; HAProxy 121,704). User instructions per request fall from 25.7k in 6.5.2 (23.9k on the tokio plane) to
  14.7k (nginx 19.9k).
- **Large bodies are spliced.** A 64 KiB response: 87,002 requests per second against 48,338 on the tokio plane and
  35,542 for nginx (2/2 clean each).
- **New connections are cheaper.** One request per connection: 88,643 requests per second against 71,366 on the tokio
  plane, 74,708 for nginx and 67,016 for HAProxy.
- **High connection counts hold their rate.** A request start is paced (8 per pass over ready events), so the requests
  in flight no longer grow with the connection count. 1,024 connections (earlier pass): 164,201 requests per second against
  109,769 before, nginx 85,379; 2,048 connections: 131,046 against 55,289, nginx 59,882. At 1,024 connections the final
  tree measured 170,641 against nginx 99,152.
- **TLS handshakes cost 2.5 times fewer instructions.** rustls with aws-lc is the default backend: a full TLS 1.3
  handshake costs 665,649 user instructions (nginx 1,559,140; 1,650,828 with OpenSSL in the earlier pass) and runs at
  16,108 handshakes per second against 5,397 for nginx and 4,751 for HAProxy (earlier pass: OpenSSL 5,821). Keep-alive
  TLS 1.3 gained about 15% over OpenSSL in the earlier pass (105,040 against 91,429 requests per second); the final
  tree measured 108,694 against 101,283 for nginx (one clean OxiRoute run, indicative).
- **Kernel TLS and `sendfile` work with rustls.** A 1 MiB static file over TLS 1.3: 4,169 requests per second against
  2,195 for nginx without kernel TLS. rustls connections follow client key updates and roll keys before the AES-GCM
  record limit.
- **The cache serves hits on the fast lane from a sharded store, and keeps compressed variants.** A 1 KiB hit runs at
  231,274 requests per second against 128,691 for nginx (one clean nginx run, indicative), and 223,853 against 113,185
  before (A/B on one host, earlier pass); a 64 KiB hit is level with nginx (97,425 against 99,966). A cache policy can now store a body once per coding (`zstd`,
  `br`, `gzip`) and serve hits with the best coding the client accepts; no throughput claim is made for that feature
  beyond the report's single-invocation rows.
- **Memory.** Idle PSS is 13.0 MB (63.8 MB before), 24.2 MB with 1,024 connections held (82.1 MB before); the
  embedded allocator sets `thp:never` and buffers start small and grow on demand. An idle TLS connection holds 15.0 kB
  with rustls against 32.4 kB with OpenSSL.
- **An idle process is quiet.** Timer wake-ups of an idle direct-runtime process fall from about 100 per second to 0.1;
  the proxied request reads the clock once per batch of events.
- **HTTP/3 uses BBR by default.** A 10 MiB download at a 40 ms round trip with 0.5% loss takes 0.37 s against 15.0 s
  with `cubic`. The costs are in [Defaults that changed](#defaults-that-changed) and are not small.
- **Packages.** Debian and RPM packages for `amd64`/`x86_64` and `arm64`/`aarch64`, binary tarballs and one
  `SHA256SUMS` are built from one profile-guided binary per architecture (see [Install](#install)).

## Defaults that changed

| Default | 6.5.2 | 7.0.0 | Restore the old behaviour |
| --- | --- | --- | --- |
| HTTP/1 data plane (`http_data_plane`) | every request on the tokio/Pingora path | `epoll`, then `tokio`, chosen per listener when the setting is omitted | `http_data_plane = "tokio"` |
| TLS backend (`policy.backend` of a TLS profile) | `openssl` | `rustls` (aws-lc), unless the profile needs OpenSSL | `backend = "openssl"` on each profile |
| HTTP/3 congestion control (`http3_congestion_control`) | `cubic` | `bbr` | `http3_congestion_control = "cubic"` |
| Transparent huge pages in the allocator | jemalloc default | `thp:never` | `_RJEM_MALLOC_CONF=thp:default` |

### Data plane: epoll by default

- With `http_data_plane` omitted, each plain-text HTTP/1.1 listener runs on the epoll plane when the platform supports
  it and `http_fast_lane` is not `false`; otherwise it stays on the tokio path. The choice is made once per generation
  and logged once per listener at info level with the reason each better plane was skipped. A listener that terminates
  TLS keeps the tokio path unless `http_data_plane = "epoll"` is set and its profile uses rustls (the plane then serves
  HTTP/1.1; HTTP/2 connections continue on the tokio path over the same TLS session); HTTP/3 and Unix-socket listeners
  always take the tokio path.
- Requests the engine cannot prove equivalent (upgrades, chunked request bodies, `Expect`, routes the fast lane does not
  serve, static and fixed-response routes) are handed to the tokio path with the bytes already read.
- **Behaviour difference.** The engine planes serve requests that a client sends back to back on one connection
  (pipelining) one after the other, where the classic path closes the connection after the first answer.
- **Thread names.** Engine threads are named `epoll-h1-N` (or `uring-h1-N`). Plain HTTP/1.1 traffic is served by them
  instead of by the tokio worker threads, so per-thread monitoring, CPU pinning by thread name and profiles need the new
  names. If an engine thread panics or fails, the generation is marked failed and the
  supervisor replaces the worker.
- An explicit `epoll` or `io_uring` is never downgraded: the configuration is rejected, naming the missing capability,
  when the kernel, sandbox or `http_fast_lane = false` cannot support it (`io_uring` is also rejected with
  `connection_scheduling = "aged_per_worker"`).
- `io_uring` is opt-in (`http_data_plane = "io_uring"`) and is never part of the default order. It lives in the new
  `oxiroute-uring` crate, the only place with `unsafe` for it; `scripts/verify/unsafe_containment.sh` enforces that.
  It matches epoll at 1 KiB keep-alive with 32 connections (170,511 against 169,706), is 2% below at 256 and 12% below
  at 1,024 connections, costs 4.1k to 5.9k more user instructions per request, and is slower at 64 KiB (58,404 against
  87,002), so epoll stays the default.
- `connection_scheduling = "aged_per_worker"` (and `auto` from two workers) and `worker_scaling = "dynamic"` now run
  on the epoll plane's own threads and keep their defaults (`work_stealing`, `static`).

### TLS: rustls is the default backend

- A profile that omits `backend` runs on rustls with the aws-lc-rs provider, chosen once per generation when the
  configuration is compiled, never at run time. It gets OpenSSL instead, with one info line naming the setting, when it
  sets `dh_parameters_path`, lists a `DHE-RSA-` or `EDH-RSA-` suite in `cipher_list`, or has a `cipher_list` that
  selects no TLS 1.2 suite rustls implements while `min_version` is `1.2`. `backend = "openssl"` always selects
  OpenSSL; `backend = "rustls"` with one of those settings is a configuration error that names the setting. A
  connection never changes library.
- **The default TLS 1.2 list no longer offers finite-field DHE-RSA suites.** A client that can only negotiate those fails
  the handshake. Serve it from a separate listener and profile with `backend = "openssl"` and the suites in
  `cipher_list` (see "Serving legacy clients" in the [operations guide](OPERATIONS.md)).
- **No FIPS module on rustls.** Upstream TLS, ACME and RTMPS keep using OpenSSL whatever the setting; HTTP/3 uses its
  own stack.
- `policy.backend` stays optional: an omitted value is kept omitted when a configuration is rendered, imported or edited
  in the dashboard. `oxiroute import nginx` lowers `ssl_ciphers` macros (`HIGH:!aNULL`) to the default policy with a
  note naming the dropped suites and the legacy-client recipe; `ssl_dhparam` selects OpenSSL by the rules above.
- Certificates, SNI selection, ALPN, the minimum version, client authentication, session cache and tickets,
  `key_exchange_groups`, certificate hot reload and TLS-ALPN-01 behave the same on both backends.

### HTTP/3: BBR by default

Set `http3_congestion_control = "cubic"` on a listener whose clients are on a LAN or in a data center, that shares a
bottleneck with traffic that must not be starved, or where CPU per MiB or packet loss matters more than the rate on
lossy paths. The costs of `bbr`, measured on private network namespaces and checked in under
[benchmarks/r2-data-path/fairness](../benchmarks/r2-data-path/fairness/results/summary.md):

- A loss-free LAN 48 MiB download runs at 226 MiB/s instead of 1,100 MiB/s (the controller's window stays at its floor
  when the round trip is tens of microseconds). The LAN 1 KiB request rate is unchanged.
- Against a `cubic` HTTP/3 flow or a TCP flow on a shared 100 Mbit/s link, a `bbr` connection takes 99.3% to 99.7%
  of the link (Jain's index 0.50). It is not fair to other flows.
- Alone through a shallow-buffer bottleneck it loses 28% to 70% of its packets and uses 1.7 times the CPU per MiB
  (0.8 to 2.0 across cells).
- The gain is on lossy or long paths: 10 MiB takes 0.37 s against 15.0 s at 40 ms and 0.5% loss, and 0.80 s against
  61.8 s at 100 ms and 1% loss.

The change is an ordinary reload; established connections keep their controller. The related
`http3_segmentation_offload` (default `true`) hands the kernel several datagrams per `sendmsg` (UDP segmentation
offload); set `false` for a NIC, driver, virtual network or shaper that mishandles segmented datagrams.

### Allocator: `thp:never`

On hosts with transparent huge pages enabled for every mapping, jemalloc's 2 MiB extents became huge pages and an idle
process held tens of MiB it never touched; the embedded configuration now sets `thp:never`. A build without jemalloc
(`--no-default-features`, `std::alloc::System`) is supported and documented in
[Allocator selection](developer/ALLOCATORS.md).

## Added configuration

| Key | Where | Default | Notes |
| --- | --- | --- | --- |
| `http_data_plane` | root | omitted: `epoll`, then `tokio` | `epoll`, `io_uring` or `tokio`; explicit values are never downgraded |
| `http3_congestion_control` | root | `bbr` | `bbr` or `cubic` |
| `http3_segmentation_offload` | root | `true` | boolean |
| `policy.backend` | TLS profile | omitted: `rustls` unless the profile needs OpenSSL | now optional; was `openssl` |
| `compression` | reverse HTTP cache policy | absent: bodies stored as sent | `codings`, `precompress`, `content_types`, `min_length_bytes`, `gzip_level`, `brotli_level`, `zstd_level` |

`connection_scheduling`, `connection_age_ms` and `worker_scaling` are unchanged in meaning and default; the epoll plane
honours them. The full rules are in the [configuration specification](CONFIG_SPEC.md).

## Upgrade guide from 6.5.x

1. Read [Defaults that changed](#defaults-that-changed). Nothing in an existing configuration file needs editing to
   start; every changed default can be pinned to its 6.5 value with the setting in the table.
2. To change nothing observable at first, set `http_data_plane = "tokio"`, `http3_congestion_control = "cubic"` and
   `backend = "openssl"` on every TLS profile, reload, then move one listener at a time.
3. Check the info line of each listener at the first reload: it names the chosen plane and the reason for each skipped
   one. Check the log of each TLS profile for the backend line and for any cipher-list warning about suites that rustls
   leaves out.
4. Update monitoring that matches thread names, and any CPU pinning of Pingora worker threads, for `epoll-h1-N`.
5. Packages restart a running service on upgrade (distribution policy). Edited files in `/etc/oxiroute` are kept.
6. Kernel TLS still needs the `tls` kernel module (kernel 6.5 or newer; 6.14 or newer for TLS 1.3 key updates on
   rustls); see [Kernel TLS](OPERATIONS.md).
7. Rust code that uses the `oxiroute-config` crate sees `TlsPolicy::backend` as an `Option` and new fields on the root
   configuration; the public API baselines have not been regenerated (see [Known limits](#known-limits)).

## Build, packaging and CI

- **Release assets.** Debian packages for Debian 12 and 13 and Ubuntu 22.04 and 24.04 (`amd64`, `arm64`), RPMs for
  the RHEL family 9 (`el9`) and Amazon Linux 2023 (`x86_64`, `aarch64`), binary tarballs for `x86_64` and `aarch64`, the
  source archive and one `SHA256SUMS`. All of an architecture's assets repackage one binary built on an `almalinux:9`
  container (glibc 2.34 floor, checked on every build). The packages enable and start nothing. Attestations are produced
  for tarballs, packages and the source archive.
- **Faster pipeline.** The PGO profile is cached under a key made of the version and a hash of the toolchain, build
  settings and sources; a `Profile refresh` job keeps it warm on `main`, so a tag restores it instead of training. A version bump needs a new profile: wait for `Profile refresh` before tagging
  ([Releasing](developer/RELEASING.md)). The estimated critical path of a tag is about 29 minutes on hosted runners,
  an extrapolation from a local measurement of 350 s for the profile-use build.
- **CI.** Test binaries are listed once with the tiers they run in (`scripts/ci/tiers.txt`, checked by
  `scripts/ci/tiers.sh check`); focused lanes run per data plane (tokio, epoll, io_uring), TLS and core suites, and a
  nightly lane runs the slow tests.
- **Repository structure rules, enforced by checks.** Tracked source files stay within 1,000 lines
  (`scripts/verify/line_limit.sh`) and sibling files share no name prefix (`scripts/verify/name_prefixes.sh`); both
  ratchet against recorded baselines that only shrink. Suites and sources that violated them were split into
  folders named by topic, with no behaviour change.
- **Build option.** `--no-default-features` builds the server with the system allocator.
- **Tests.** The HTTP/1 wire suites run on every data plane the kernel allows (tokio, epoll, io_uring).

## Install

Every release attaches its assets to the [GitHub release](https://github.com/brauliobo/oxiroute/releases). There is no
hosted apt or yum repository: download the file you need and `SHA256SUMS` into one directory, then verify and install.

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <asset> --repo brauliobo/oxiroute          # optional

sudo apt install ./oxiroute_7.0.0_amd64.deb                      # Debian 12/13, Ubuntu 22.04/24.04 (arm64: _arm64.deb)
sudo dnf install ./oxiroute-7.0.0-1.el9.x86_64.rpm               # AlmaLinux and Rocky Linux 9 (aarch64: .el9.aarch64.rpm)
sudo dnf install ./oxiroute-7.0.0-1.amzn2023.x86_64.rpm          # Amazon Linux 2023 (aarch64: .amzn2023.aarch64.rpm)

tar -xzf oxiroute-7.0.0-x86_64-unknown-linux-gnu.tar.gz          # any glibc 2.34 or newer Linux
sudo install -m 0755 oxiroute-7.0.0-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
```

[Install a release package](user/INSTALL.md) has the service opt-in and the upgrade, remove and purge behaviour. The
Arch packages (`oxiroute`, `oxiroute-bin`) are described in the [Arch packaging guide](../packaging/arch/README.md);
the binary package's checksum is pinned after the release assets exist.

## Known limits

- **Measurement conditions.** The performance tables are from builds without profile guidance (the final tree for the
  headline rows, the integration build for rows marked earlier), one worker per proxy, loopback, closed loop, on a
  shared host. Rows with fewer than two clean runs are marked indicative; the nginx row of the 32-connection lane and
  the HTTP/3 rows have one clean run. The nginx and HAProxy harness is a local
  script that is not in the checkout, so those columns cannot be reproduced from the repository; the OxiRoute
  instruction counts reproduce with `benchmarks/r2-data-path`. nginx and HAProxy were not tuned.
- **io_uring.** It does not beat epoll at 1 KiB keep-alive requests, uses more user instructions per request, is slower
  with 64 KiB bodies, does not serve TLS and does not run `aged_per_worker`. It stays opt-in.
- **64 KiB cache hits** are level with nginx, not faster; the compressed-variant lanes are single invocations.
- **Epoll and TLS.** An HTTPS listener runs on epoll only when `http_data_plane = "epoll"` is set and its profile uses
  rustls; HTTP/2 stays on the tokio path.
- **BBR** is not fair to other flows and is slower on a loss-free LAN (see above).
- **Public-API baselines.** The checked rustdoc baselines and deltas under `docs/developer/fixtures` and the
  checks `scripts/verify/public/api.sh`, `scripts/verify/public/api-baseline-provenance.sh` and
  `scripts/verify/generation/blueprint/baseline.sh` need the pinned toolchain through rustup (Rust 1.97.1); they were
  not run for this release and the fixtures were not regenerated. The version bump changes none of them (they are keyed
  by commit, not version).
- **Release workflow.** The aarch64 binary, packages and the profile-guided build run only in the release workflow; the
  workflow's first aarch64 run may need additions to the instruction allow-list described in
  [Releasing](developer/RELEASING.md).
- **Compatibility claims** are unchanged; see the [compatibility matrix](COMPATIBILITY.md).
