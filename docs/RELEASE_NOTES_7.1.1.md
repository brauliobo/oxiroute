# OxiRoute 7.1.1

Prepared 2026-11-19 for the `v7.1.1` tag.

OxiRoute 7.1.1 is a patch release of 7.1.0. It rebuilds `oxiroute top` as a htop and btop style dashboard, tracks every
client and connection of the public listeners behind two new management endpoints, and attaches an Arch Linux package
to each release. An existing configuration file needs no edit; the two things worth reading before you upgrade are
the new `top --json` schema and the small cost of tracking (see [Upgrading](#upgrading) and
[Performance](#performance)).

## Headlines

- **A redesigned `oxiroute top`.** Live sparkline meters, an upstream tree, clients, connections, in-flight requests,
  RTMP streams and generations, mouse support, rate windows of 2s, 10s and 60s, and a layout you can collapse, hide and
  remember per user.
- **Per-client and per-connection tracking.** `GET /api/v1/clients` and `GET /api/v1/connections` show who is connected,
  over which listener and protocol, and how much each peer moved, for every public listener kind.
- **An Arch Linux package on every release.** The release builds `oxiroute-VERSION-1-x86_64.pkg.tar.zst`, installs it in
  a fresh Arch container, and attaches it; `oxiroute-bin` on the AUR installs that very package.

## `oxiroute top`

The reference is [Live dashboard](MANAGEMENT_CLI.md#live-dashboard) and [reference/CLI.md](reference/CLI.md#top).

- **Views.** Listeners, an expandable upstream tree (pools with their servers, health, breaker and outlier state),
  Clients, Conns, Requests, RTMP and Generations, in a tab strip with badges (unhealthy, down, overdue, draining).
  `--view` starts in one of them.
- **Live meters.** A panel of sparklines with peak and total for requests and bytes in and out, another for the load
  (CPU, connections, memory, descriptors), a summary panel per view and a detail drawer with the history of the selected
  row. Rates are measured over a 2s, 10s or 60s window (`--window`, or `w`) or since start (`life`).
- **Layout.** Panels, groups, columns and views can be collapsed or hidden, columns appear by priority as the terminal
  narrows and are never truncated, and the choice is remembered per user in
  `$XDG_STATE_HOME/oxiroute/top.json` (`--reset-layout` forgets it).
- **Interaction.** Mouse and keyboard selection, sorting by column, an incremental filter (`ip:PREFIX` and
  `listener:NAME` are also sent to the server), a column picker, themes (`dark`, `light`, `mono`), `--ascii` and
  `--no-mouse`.
- **Output.** `--once`, `--json` and `--csv` take two samples one window apart and print one measured view. `--json` is
  schema version 2: the view's rows with their units (schema version 1 was the raw `GET /api/v1/stats` document).
  Whole counters print as integers.
- **Narrow terminals.** A narrow status line drops whole segments instead of cutting a word, and meters no longer flicker
  between `10.0KB/s` and `10KB/s`.

## Client and connection tracking

The coverage table and the accounting rules are in
[Client and connection tracking](OPERATIONS.md#client-and-connection-tracking); the endpoints are in
[the API reference](reference/API.md#clients-and-connections).

- **`GET /api/v1/clients?limit=&sort=&ip=`** lists peers aggregated by address with live and total connections,
  requests, bytes in and out, errors, listeners and protocols, plus an `other` bucket and `totals`.
- **`GET /api/v1/connections?limit=&ip=&listener=`** lists every open connection with its peer, listener, protocol,
  TLS version, SNI and ALPN, state, age, idle time, counters, upstream server and current request.
- **Every public listener kind is tracked:** HTTP/1 (plain and TLS), HTTP/2 and WebSocket on the epoll and tokio
  planes, HTTP/3, UDP flows, L4 TCP relays, RTMP sessions and the forward proxy (HTTP/1, HTTP/2, HTTP/3, CONNECT).
  Control listeners and the stats page listener are not.
- **`client_tracking`** is a new optional section: `enabled` (default `true`), `max_clients` (default 10000; the least
  recently active clients beyond it fold into `other`, so totals stay exact) and `ipv6_prefix` (`128` by default, `64`
  groups a subnet). A reload applies it to the new generation. When disabled the endpoints answer `409
  client_tracking_disabled` and the data path does no tracking work.
- **Privacy.** Peer addresses are served only by these two authenticated endpoints. They never appear on the public
  stats page, in `/metrics` or in events.
- **Vendored Pingora.** The connection digest of the vendored `pingora-core` carries one more slot for the tracking
  record, dropped with the connection ([the patch notes](../vendor/pingora-core/README.oxiroute.md)).

## Fixed

- **A `101` upgrade counts as a success** in the listener statistics.
- **A rotated management token** is taken by the daemon at its next reload (`generation reload` with the old token, or
  `SIGHUP`) or restart, as the token documentation and the `token rotate` help now say; the CLI uses the new file at
  once.
- **Arch guide.** The loopback address in the packaged Arch guide was misprinted as `127.1.0.1`; it is `127.0.0.1`.
- **Tests.** The unit-test port allocator is exclusive across processes (internal).

## Performance

7.1.1 changes no hot path beyond the tracking record, so the 7.1.0 numbers in [the benchmarks](BENCHMARKS.md) and the
[7.1 report](../benchmarks/reports/7.1/README.md) stay and were not repeated. Measured against 7.1.0 with the
comparison harness, tracking costs about 1.9% of the connect-per-request throughput (6.6% more instructions per new
connection, the cost of creating and registering the record); HTTP/1 keep-alive and HTTP/2 are unchanged within noise.
`client_tracking.enabled = false` removes the cost.

## Upgrading

Nothing in an existing configuration file needs editing to start.

1. **`oxiroute top --json` changed shape.** It prints schema version 2 (rows and units per view). A script that read the
   raw statistics document from `top --json` should read `GET /api/v1/stats` instead.
2. **Tracking is on by default.** Set `client_tracking.enabled = false` (or the same setting in your configuration format) to turn it off. Connection counts and addresses
   are held in memory only, bounded by `max_clients`.
3. **Arch Linux.** `oxiroute-bin` now installs the release's own `oxiroute-VERSION-1-x86_64.pkg.tar.zst` instead of
   repackaging the tarball. The `oxiroute` source recipe is unchanged. The package, like the other families, enables and
   starts nothing.

## Build, packaging and CI

- The release assets are those of 7.1.0 with the version `7.1.1`, plus the Arch package of `x86_64`
  (`oxiroute-7.1.1-1-x86_64.pkg.tar.zst`, its `.sha256` and `.build.json`). One profile-guided binary per architecture is
  repackaged into every tarball and package, and `SHA256SUMS` lists them all.
- The Arch package is assembled with `makepkg` in `archlinux:base-devel`, installed with `pacman -U` in `archlinux:latest`
  and checked against the files of the other packages before `publish` attaches it.

## Install

Every release attaches its assets to the [GitHub release](https://github.com/brauliobo/oxiroute/releases). There is no
hosted apt or yum repository: download the file you need and `SHA256SUMS` into one directory, then verify and install.

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <asset> --repo brauliobo/oxiroute          # optional

sudo apt install ./oxiroute_7.1.1_amd64.deb                      # Debian 12/13, Ubuntu 22.04/24.04 (arm64: _arm64.deb)
sudo dnf install ./oxiroute-7.1.1-1.el9.x86_64.rpm               # AlmaLinux and Rocky Linux 9 (aarch64: .el9.aarch64.rpm)
sudo dnf install ./oxiroute-7.1.1-1.amzn2023.x86_64.rpm          # Amazon Linux 2023 (aarch64: .amzn2023.aarch64.rpm)
sudo pacman -U ./oxiroute-7.1.1-1-x86_64.pkg.tar.zst            # Arch Linux (x86_64 only; AUR: oxiroute-bin)

tar -xzf oxiroute-7.1.1-x86_64-unknown-linux-gnu.tar.gz          # any glibc 2.34 or newer Linux
sudo install -m 0755 oxiroute-7.1.1-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
```

[Install a release package](user/INSTALL.md) has the service opt-in and the upgrade, remove and purge behaviour. The
Arch packages (`oxiroute`, `oxiroute-bin`) are described in the [Arch packaging guide](../packaging/arch/README.md).

## Known limits

- **Tracking scope.** Only the active generation is reported; in the direct (unsupervised) runtime connections of
  draining generations in the same process appear too. Bytes are payload bytes, not wire bytes. The tokio plane reports
  no SNI, forward HTTP/3 CONNECT tunnels credit their bytes when the tunnel ends, and RTMP sessions carry no
  application, stream or role. TCP and forward connections are keyed by the socket peer, not by a PROXY-protocol address.
- **`top`.** There is no Routes view: the statistics keep no per-route counters. A server without the new endpoints (an
  older daemon) keeps every other view and says so on one line in Clients and Conns.
- **The 7.1.0 limits** (`oxiroute upgrade`, state a reload does not carry, FLV recordings, RTMP exec profiles, overlay
  promotion, the event ring) are unchanged; see [the 7.1.0 notes](RELEASE_NOTES_7.1.0.md#known-limits).
- **Compatibility claims** are unchanged; see the [compatibility matrix](COMPATIBILITY.md).
