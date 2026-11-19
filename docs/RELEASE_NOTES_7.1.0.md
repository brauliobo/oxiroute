# OxiRoute 7.1.0

Prepared 2026-11-19 for the `v7.1.0` tag.

OxiRoute 7.1.0 is an operations release. It makes the daemon manageable from a terminal in its own configuration
formats, lets a configuration reload apply nearly everything without a restart and carry state across it, adds a
zero-downtime binary upgrade, rebuilds the web console on one design system, and gives RTMP recordings an inventory,
retention and a browser download path. The data path of 7.0.0 is unchanged; the performance tables stay in
[the benchmarks](BENCHMARKS.md) and are updated after the release.

A few CLI spellings of 7.0.0 changed (see [Upgrading](#upgrading)); an existing configuration file keeps working
without an edit.

## Headlines

- **Configuration from the CLI, in the file's own format.** `config validate|diff|apply|edit|fmt` read and write KDL,
  Lua, HOCON, UCI and JSON; comments and Lua logic survive; every save keeps the text it replaced and
  `config rollback` restores it.
- **Changes that survive a restart.** `--persist` writes a runtime change (drain, weight, limits, checks) to an
  overlay file beside the configuration; every mutation says where its change lives.
- **A reload applies the whole file.** Settings that cannot change live no longer block the rest; caches, health,
  tickets and ACME state follow the reload; every replaced worker drains on its own.
- **`oxiroute upgrade`.** A running daemon hands its listening sockets to a new binary without refusing a connection.
- **A new web console.** One shell, one login, light and dark themes, field-level validation, a diff review before
  saving, and a Cameras workspace with live previews and recordings.
- **RTMP management.** A recordings inventory with quotas and retention, short-lived media tickets for browser
  playback, an ended-stream catalog, and CLI commands for sessions and recordings.
- **First-run and diagnosis commands.** `init`, `token`, `doctor`, `route test`, `top`, `tls check`, `audit`,
  contexts, shell completions and a manual page.

## CLI

The full reference is [reference/CLI.md](reference/CLI.md); every command and option has `--help` with examples.

- **Configuration commands in native formats.** `config check FILE` (offline), `config validate FILE` (the daemon
  resolves the file exactly as it loads its own and stores nothing), `config diff FILE` (settings by name, such as
  `/upstream_pools/web/servers/origin-1/max_connections`, secrets redacted, `--text`, `--exit-code`), `config apply
  FILE` (`--dry-run`, `--wait`, `--convert`, revision precondition), `config edit` (`$EDITOR` on the stored text,
  validate, review, confirm, apply), `config show` (stored text, or the running configuration with `--active`) and
  `config fmt` (`--check`, `--write`, `--diff`). A refused file exits `3` with each diagnostic's code, `FILE:LINE`,
  setting and a `hint:` line. A daemon that cannot write its file is refused up front with exit `8` and the fix.
- **History and rollback.** Every save through the daemon (`config apply`, `config edit`, the console) keeps the
  replaced text in `<configuration file>.history/` (`0600` files in a `0700` directory), bounded by the new
  `config_history` setting (20 versions and 16 MiB by default). `config history`, `config show --rev ID`, `config diff
  --rev A [--rev B]` and `config rollback ID [--wait]` list, read, compare and restore them; a rollback is a normal
  validated apply and is itself kept. Edits made directly on disk are not kept.
- **Overlay and `--persist`.** `--persist` on `listener`/`pool`/`server` `ready`, `drain`, `maintenance`,
  `set-health`, `check`, `max-connections`, `weight` and `evacuate` writes the change to
  `<configuration file>.overlay.json` (mode `0600`) before applying it, and it is applied again at every start and
  reload. `overlay show` reports each setting as `applied`, `diverged` or `orphan`, `overlay clear` deletes the file and
  `overlay promote` writes the connection limits into the configuration file in its own format.
- **Contexts.** `context list|add|use|remove|current` name daemons with an address, a token source (a file, an
  environment variable or a command, never the token itself) and a default timeout, selected with `--context`,
  `OXIROUTE_CONTEXT` or the current one. Flags beat variables, which beat the context, which beats discovery of the
  packaged setup; a context's token is never sent to an endpoint given by flag or variable.
- **Waiting and timeouts.** `ready --wait`, `generation reload --wait` and `generation drain --wait` block until done;
  `--timeout` bounds the connection and each answer, and a silent daemon is reported as a timeout with the limit.
- **Output.** `--output table` prints aligned columns with units and short revisions, `--output plain` one `key=value`
  record per line, and `--output json` the management API body unchanged. Mutations print their `persistence` and
  whether each target really changed; an empty result prints `no <things>` on stderr and exits `0`.
- **`tls`.** `list` and `show` print expiry in UTC with the time left (`--expiring-within`), `inspect HOST:PORT` reads the
  certificate any TLS server presents, `check` is a monitoring plugin with exit codes `0`/`1`/`2`/`3`, and `renew`,
  `revoke`, `delete` and `acme rollover|pause|resume|cancel` reach the ACME endpoints the console uses.
- **`rtmp`.** `publisher` and `viewer` `list|kick`, `stream disconnect` (exit `9` in 7.0.0), and `recording
  list|show|download|delete|quota` (see [RTMP management](#rtmp-management)).
- **`events` and `audit`.** `events follow` reads the server-sent event stream, reconnects from the last event it
  printed, and takes `--after`, `--since` and `--count`; `audit list|status|tail` query the redacted audit log by
  category, result and window.
- **`route test URL`** (or `HOST PATH`) asks the daemon which route, action and pool would serve a request on each HTTP
  listener, without sending it; exit `6` when none does.
- **`doctor [CONFIG]`** checks the configuration, listen addresses, upstream reachability, certificate expiry, file
  modes and whether the systemd unit's sandbox lets the daemon write what the configuration writes, with the fix for
  each finding and monitoring exit codes.
- **`init proxy|static|rtmp`** writes a minimal configuration that loads, validates and prepares, in KDL, Lua, UCI or
  HOCON, and **`token create|rotate`** writes or atomically replaces the management token file without printing it.
- **`top`** is a read-only live terminal dashboard of listeners, pools, servers and in-flight requests with rates,
  latency percentiles, sorting and filtering, over the new management endpoint `GET /api/v1/stats`; `--once` and
  `--json` print one snapshot.
- **`completions bash|zsh|fish` and `man`** print completion scripts and the manual page for `oxiroute` and `oxr`;
  the Arch, Debian and RPM packages install both names.
- **Safer invocation.** A mistyped command is a usage error with suggestions instead of starting the daemon, and every
  failure names the endpoint, the file and what to fix.

## Reload instead of restart

The operations contract is [OPERATIONS.md](OPERATIONS.md#settings-that-wait-for-a-restart).

- **Listeners and sockets.** A Unix listener that keeps its path takes a changed `mode` in place. A changed
  `listener_workers` of an HTTP/3 listener is applied on the kept UDP address (HTTP/3 listeners now always bind with
  `SO_REUSEPORT`). A configuration may have up to 253 listeners (the kernel's limit for one descriptor message); above
  that the packaged service stops at start or refuses the reload with an error naming the limit.
- **Settings that wait for a restart no longer block the rest.** A reload keeps the running value of `ipv6_only` of a
  kept address (and, in the direct runtime, the `registry` block and `cpu_level`), applies everything else, and
  reports each waiting setting once as `restartRequired` in `generation status`, `status`, `/api/v1/generations`,
  `config validate|apply`, the `oxiroute_restart_required` gauge and `oxiroute check`. The generation is not degraded
  and `/ready` is unaffected.
- **External files.** A rewritten management or statistics token, registry tokens file, route credentials, client CA,
  upstream CA, `dh_parameters_path` or RTMP secret file is part of the candidate revision, so the next reload replaces
  the generation without an edit of the configuration.
- **Store ownership handover.** A disk cache store and a managed ACME state root have one writer at a time; the lock
  moves when the replacement activates, so the cache stays warm and renewal continues. A replacement that cannot take
  the store is rejected with a message naming it and the active worker takes it back.
- **State continuity.** Server health, passive ejections and circuit breakers carry over by server name and health
  checks do not all fire at once; TLS session tickets (rustls) and the RTMP media ticket key stay valid; in the direct
  runtime the memory cache and TLS session cache are taken by the new generation. The matrix of what survives is in
  [State that survives a reload](OPERATIONS.md#state-that-survives-a-reload).
- **Build-id skew.** Every process knows its build id (`gnu:<hex>`); a replacement worker of another build than the
  master (what a package upgrade leaves behind) is refused with both ids and a pointer to `oxiroute upgrade`, and the
  active worker keeps serving. `status` shows `build_id` and, under a master, `master_build_id`.
- **Concurrent retired workers.** Under the supervising master each replaced worker drains on its own, up to
  `retirement.max_retired_generations`; a reload past the limit closes the oldest draining worker (reported with
  reason `retired_limit`) instead of waiting. A reload that waits shows `pendingApply` with the reason and the
  connections left.
- **`cpu_level`.** The instruction-set level of the byte scanners is a root setting (`scalar`, `sse2`, `sse42`, `avx2`,
  `avx512`) that replaces the `OXIROUTE_CPU_LEVEL` fallback; a supervised reload applies it in the new worker. Workers
  now also inherit `RUST_LOG`, `OXIROUTE_CPU_LEVEL`, `_RJEM_MALLOC_CONF`, `SSL_CERT_FILE` and `SSL_CERT_DIR`.
- **Zero-downtime `oxiroute upgrade [--config PATH] [--binary PATH] [--wait]`.** The running master starts the new
  executable as its successor and hands it the listening TCP, UDP and QUIC sockets over a private channel; the
  successor loads the configuration from disk, checks that its listeners match, and takes over once its worker is
  active, while the old worker finishes its connections. A successor that exits or is not ready within 60 seconds is
  ended, the command fails with the reason and the old master keeps serving. See
  [Binary upgrade](OPERATIONS.md#binary-upgrade).

## Web console

The console is built on one design system shared with the statistics page ([UI design system](developer/UI_DESIGN_SYSTEM.md)).

- **Design system.** One light and dark token set with a contrast-tested lime brand, shared primitives (buttons,
  fields, tables, badges, dialogs, toasts, tabs, shell), an accessibility check of every primitive in both themes and
  densities, and CI rules that block raw colours, spacing and cross-imports.
- **Shell, login and themes.** One shell with a skip link, a title per view, automatic, light or dark themes, and one
  login that keeps the token for the tab session only (never in cookies, `localStorage` or the URL); no API request is
  sent before it exists, a rejected token is told apart from a missing one, and destructive actions ask in a dialog
  that names the target.
- **Configuration workspace.** Validation errors appear next to the offending field; the save review shows the changed
  lines of the rendered file; when the file changed under an unsaved draft, "Reload and reapply my edits" merges the
  draft onto the new file by setting and lists collisions; the recorder retention limits are editable; no false
  "revisions differ" warning on a fresh start.
- **Operations.** Action results stay next to their section, server weight, evacuation (with force), the in-flight
  requests list, registrations, capabilities and the restart-required banner are available, and every drain, reload,
  rollback or shutdown asks first.
- **Lists.** Certificates (UTC times, expiry badges), Events (text and name filters, restarted-ring marker, Live,
  Reconnecting or Offline), Audit (table, degraded-store reason), Provenance, Overview, Topology and Statistics share
  one set of tables and status words.
- **Cameras.** A stream wall with live, idle, degraded and offline tiles, ticketed live previews (at most four at once;
  they need an HLS output and a browser that plays HLS natively), recorders and session drops, a Recordings tab with
  filters, quotas and ticketed downloads, and an Access keys tab showing key fingerprints only
  ([Dashboard guide](user/DASHBOARD.md#cameras)).
- **Delivery.** The management listener serves the console with a strict Content-Security-Policy, `nosniff`, framing
  and referrer headers, immutable hashed assets, a revalidated `index.html` (`ETag`, `304`) and Brotli and gzip
  variants prepared when the files load; a fronting proxy no longer needs to add them.

## RTMP management

The details are in the [RTMP guide](user/RTMP.md) and the [API reference](reference/API.md#rtmp-recordings).

- **Recordings inventory.** Every recording has a metadata sidecar; `GET /api/v1/rtmp/recordings` lists them with
  stream, times, size, codecs and a status of `recording`, `complete` or `partial` (after a crash); content downloads
  with `Range` support; finished recordings can be deleted (audited). Ids are stable across restarts.
- **Retention and quota.** A full recording root fails the recorder with `quota_files` or `quota_bytes` instead of a
  generic error and recovers when space is freed. The new recorder settings `retention = "delete_oldest"` and
  `retention_max_age_ms` delete the oldest finished recordings, never an active one. `rtmp recording quota` shows each
  root's usage, retention and evictions.
- **Media tickets.** `POST /api/v1/rtmp/media/{service}/{application}/{stream}/ticket` (and the VOD equivalent) mints a
  short-lived signed ticket (1 to 600 seconds, 90 by default) that a browser player uses in place of the bearer token,
  valid only for that stream's objects; it survives a reload and ends with a restart.
- **Catalog.** An ended stream stays listed as `offline` for an hour (at most 64) with its last-seen time and end
  reason; streams expose resolution, frame rate, bitrates, keyframe interval and audio format, and keep their id across
  reconnects.
- **Events, audit and secrets.** Publisher end and recorder start, stop and failure events carry stream, session and
  reason; recorder starts and stops and session drops are audited. Stream keys and callback URL credentials never
  leave in clear: see [Upgrading](#upgrading).

## Fixed

- **Stale idle upstream connections.** An upstream connection that the origin closed while it idled in the pool no longer
  costs a replayable request a retry or a passive failure: every data plane redials the same server within the same
  attempt, and a request is not replayed once the origin began answering, interim responses included.
- **io_uring.** A send the peer never drains resets the connection when the flush time is up; before, the socket could
  stay established with the peer waiting forever.
- **epoll.** With two workers or more, connections opened together are placed on the worker with the fewest open
  connections instead of staying on the worker the kernel woke to accept them. Stopping the server no longer logs the
  data plane as still referenced.
- **RTMP recorder.** With `delete_oldest` and a file quota, a segment rotation no longer fails with `quota_files` while the
  previous segment is still finishing, and eviction order no longer depends on file names when recordings share a
  modification time.
- **RTMP publishers.** FFmpeg enhanced HEVC publishers stay connected (the `colorInfo` metadata packet ended the publish
  before); other unsupported enhanced packet types now fail with an explicit reason.
- **Drain and shutdown.** `generation drain` no longer makes the management API unreachable; `shutdown` stops a
  supervised daemon and a repeated request is accepted instead of answering `409`.
- **Reload.** A refused replacement no longer changes the master's log verbosity.
- **Observability.** A start logs one line per listener, the management and statistics URLs and a `ready` line (target
  `oxiroute::startup`); an unusable audit directory is logged with its requirement and degrades `status`; a lost
  serving worker is logged at error level with its exit status before the master exits.
- **`check`.** A pool that takes registrations and has no servers yet is OK with a note, not CRITICAL.
- **`doctor`.** A `ReadWritePaths` entry that systemd creates itself no longer reads as a missing path.

## Tests, CI and release path

- **Faster tests.** Tests that never choose a data plane run once in their own tier instead of three times in the
  plane legs, a plane-less test now fails the plane legs (`scripts/ci/plane_use.sh`), the io_uring leg runs as many tests
  at once as the others, and many tests wait for events instead of the clock.
- **Faster pipeline.** Test legs start as soon as the archive is uploaded, the UI job no longer builds the management
  process, one browser job runs the dashboard, stats page and gallery tests, and the core leg no longer installs Node.
- **CPU-keyed caches.** The native-CPU lane no longer links dependency objects built for another CPU model, and a check
  rejects a workflow that would do so.
- **Release path.** A tag publishes only a commit whose `ci.yml` run passed (a new `ci-gate` job), and the tag run
  skips the checks that run already did. The shipped binary is tested by the tests that start it, taken from the debug
  test archive, instead of rebuilding every test in release mode. The package upgrade test installs the new package over
  the old one and fails when the installed build is not the new one.
- **Version bump.** As for 7.0.0, the bump on `main` starts `Profile refresh`; wait for it before tagging
  ([Releasing](developer/RELEASING.md)).

## Performance

The 7.0 report and the tables in [the benchmarks](BENCHMARKS.md) describe the data path, which this release does not
change except for the connection placement and upstream redial fixes above. The 7.1 measurements are added to
[docs/BENCHMARKS.md](BENCHMARKS.md) after the release.

## Upgrading

Nothing in an existing configuration file needs editing to start. Read these before restarting a production service.

1. **CLI spellings changed.**
   - `import ... --output report|preview` is now `--emit report|preview`; `--output` means `table|json|plain` everywhere.
     An import that still has blockers exits `1` (the report is still printed; a preview is not), where it exited `0`.
   - `generation drain --timeout-ms N` is `generation drain [--wait]` with the global `--timeout`, and
     `events follow --interval-ms` is gone (`events follow` reads the event stream).
   - Table and plain output were redesigned. Scripts should use `--output json`, which passes the API body through
     unchanged, or `--output plain`; do not parse the table.
   - An unknown first word is a usage error (exit `2`); `oxiroute CONFIG` serves only when `CONFIG` is an existing file
     or looks like a path.
   - `config validate`, `diff` and `apply` take native files (a JSON document still works) and `config apply` stores the
     file's own text, in the format of the daemon's file, instead of a canonical JSON rendering.
2. **Mutation answers report `persistence`.** Every runtime change answers `runtime` (this process only; it survives
   reloads, not a restart), `registry`, `overlay` or `mixed`. A configured server's drain is lost at a restart unless you
   add `--persist`.
3. **Pool drain and evacuate are durable for registered servers.** A pool-wide drain or evacuation now stores the drain of
   every registered server of the pool in the registry, like a per-server drain, so a heartbeat no longer undoes it and
   it outlives a restart. Undo it with `server ready` or `pool ready`.
4. **`check` and registrations.** A pool with a `registration` block and no servers yet is OK with a note unless you pass
   `--warn-available-below` or `--crit-available-below`; monitoring that relied on that CRITICAL must set a threshold.
5. **Config saves and the sandbox.** A save through the daemon keeps the replaced file's mode and, where permitted, its
   owner. `config apply`, `config edit`, `--persist`, the history and the console save write beside the configuration
   file; the packaged unit's `ProtectSystem=strict` only allows `/run/oxiroute` and `/var/lib/oxiroute`, so add
   `ReadWritePaths=/etc/oxiroute` in a drop-in (and let the service group write the directory) or edit the file on the
   host. `oxiroute doctor` and `status` (`configWritable`) say which applies.
6. **`token rotate` takes effect at the next reload.** The CLI uses the new token at once; the daemon keeps accepting
   the old one until it reloads (`SIGHUP`, or `generation reload` sent with the old token) or restarts, see
   [Reload instead of restart](#reload-instead-of-restart).
7. **`NotifyAccess=main` is added to the packaged systemd units.** `oxiroute upgrade` needs it and refuses a unit
   without it. Units that override the packaged one, such as a copy in `/etc/systemd/system`, must add it to use
   `upgrade`; the packages do not run `oxiroute upgrade` for you (run it from the post-upgrade step you choose).
8. **To change the binary without dropping connections, run `oxiroute upgrade --config /etc/oxiroute/oxiroute.kdl
   --wait` instead of restarting.** If a package replaced the binary under a daemon that was not restarted, a reload is
   refused with the build-id message above and the old worker keeps serving until you do.
9. **A Unix listener mode change is now a live reload** and no longer `restartRequired`; the only listener setting that
   waits for a restart is `ipv6_only` of a kept address.
10. **Callbacks and the config API no longer reveal RTMP secrets.** In the `query` of a callback, the values of `token`,
    `access_token`, `key`, `secret`, `password`, `passwd`, `auth`, `signature`, `sig`, `apikey`, `api_key` and the
    application's token parameter are sent as `fp-` plus eight hexadecimal characters of their SHA-256; compare
    fingerprints in the receiver. `GET /api/v1/config` shows callback URL userinfo and query values as `<redacted>`
    and restores them when the document is sent back unchanged; a placeholder that matches nothing stored is refused
    (`422`).
11. **Startup writes log lines.** One line per listener, the management and statistics URLs and `ready` appear at the
    default filter; a `log_filter` or `RUST_LOG` that sets `off` or `error` silences them.
12. **Console token.** It is kept in `sessionStorage` for the tab session (it was page memory); a fronting proxy no longer
    needs to add security headers to the console.
13. **Rust users.** `oxiroute-config` has new root fields (`config_history`, `cpu_level`) and RTMP recorder fields
    (`retention`, `retention_max_age_ms`).

## Build, packaging and CI

- The release assets and their names are those of 7.0.0 with the version `7.1.0`: Debian packages (`amd64`, `arm64`),
  RPMs for the RHEL family 9 and Amazon Linux 2023, binary tarballs for `x86_64` and `aarch64`, the source archive and one
  `SHA256SUMS`, all repackaging one profile-guided binary per architecture. The packages enable and start nothing.
- The packages now also install shell completions and manual pages for `oxiroute` and `oxr`; the Arch binary recipe
  installs them when its release tarball carries them.

## Install

Every release attaches its assets to the [GitHub release](https://github.com/brauliobo/oxiroute/releases). There is no
hosted apt or yum repository: download the file you need and `SHA256SUMS` into one directory, then verify and install.

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <asset> --repo brauliobo/oxiroute          # optional

sudo apt install ./oxiroute_7.1.0_amd64.deb                      # Debian 12/13, Ubuntu 22.04/24.04 (arm64: _arm64.deb)
sudo dnf install ./oxiroute-7.1.0-1.el9.x86_64.rpm               # AlmaLinux and Rocky Linux 9 (aarch64: .el9.aarch64.rpm)
sudo dnf install ./oxiroute-7.1.0-1.amzn2023.x86_64.rpm          # Amazon Linux 2023 (aarch64: .amzn2023.aarch64.rpm)

tar -xzf oxiroute-7.1.0-x86_64-unknown-linux-gnu.tar.gz          # any glibc 2.34 or newer Linux
sudo install -m 0755 oxiroute-7.1.0-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
```

[Install a release package](user/INSTALL.md) has the service opt-in and the upgrade, remove and purge behaviour. The
Arch packages (`oxiroute`, `oxiroute-bin`) are described in the [Arch packaging guide](../packaging/arch/README.md);
the binary package's checksum is pinned after the release assets exist.

## Known limits

- **`oxiroute upgrade`.** It refuses a configuration with a Unix socket listener (the socket file and its ownership marker
  belong to the process that bound it; restart the service instead), does not run in the direct runtime (no supervising
  master), and requires the configuration on disk to describe the running listeners. It was exercised with real
  processes and sockets, not against a real systemd. The successor starts with empty memory caches, TLS session tickets
  and cache, health and ejection state and administrative server state, and its event cursors start again at 1; a reload
  keeps them.
- **State a reload does not carry.** Under the supervising master the memory cache store and the TLS session cache start
  empty in the new worker (use a disk store to keep a cache), and TLS tickets and the session cache of the OpenSSL
  backend start empty in both runtimes (rustls tickets are carried).
- **Settings that wait for a restart.** `ipv6_only` of a kept address; in the direct runtime also the `registry` block and
  `cpu_level`.
- **Recordings are FLV.** Browsers cannot play them, so the console and the API offer downloads only; live preview needs an
  HLS output and a browser that plays HLS natively.
- **RTMP exec profiles.** Command and transcode workers cannot gain privileges and by default have no network, but are not
  sandboxed from the filesystem and have no memory or CPU limits; confine the daemon itself.
- **Overlay.** `overlay promote` writes connection limits only (states, health, checks, weights and listener states stay in
  the overlay), drops comments and logic of the file, and refuses a composed file; an unreadable overlay is ignored and
  logged, and `--persist` refuses to write until it is fixed or cleared.
- **The event ring is not durable.** After a restart or an upgrade its cursor starts again at 1, and the console
  marks the restarted ring.
- **Compatibility claims** are unchanged; see the [compatibility matrix](COMPATIBILITY.md).
