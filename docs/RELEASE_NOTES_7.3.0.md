# OxiRoute 7.3.0

Prepared 2026-11-28 for the `v7.3.0` tag.

OxiRoute 7.3.0 delivers end-to-end 7-format configuration parity across the engine, Management API, CLI, and Web UI;
directory-jailed multi-file workspace editing (`oxiroute.oxi`, `.lua`, and included files) with whole-graph validation;
format conversion and declarative resource CRUD in the CLI and API; complete structured configuration editors, revision
history with diffs and rollback, persisted overlay management, and an interactive route debugger in the Management Web
UI; and server-side hardening for overlay promotion, TLS/DNS guards, and stats admin routing. Existing 7.2.0
configurations need no edits to upgrade.

## Headlines

- **Unified 7-format configuration parity.** `.oxi`, KDL 2.0, YAML 1.2, TOML, Lua 5.4, HOCON, and OpenWrt UCI share one
  format registry (`oxiroute-config-source`) and one jailed multi-file include loader across the daemon, Management API,
  CLI, and Web UI.
- **Multi-file workspace editing (API, CLI, and Web UI).** Read, validate against the full include graph, and atomically
  save the root configuration or any included file under `/api/v1/config/files/*`, `oxiroute config files|edit|apply`,
  and the Management Web UI **Source & includes** editor.
- **Format conversion & declarative resource CRUD.** Convert between all 7 formats with `/api/v1/config/convert` and
  `oxiroute config convert`, and manage pools, listeners, HTTP services, routes, and sites declaratively via
  `/api/v1/config/patch` and `oxiroute pool|listener|route|site add|set|remove`.
- **Complete Management Web UI editors and operations workspace.** Full structured editors for all top-level config
  sections (including RTMP, L4, forward proxy, TLS/ACME, caches, and runtime tuning), revision **History & rollback**
  with unified diffs, **Overlays** inspection/clear/promotion, interactive **Route debugger**, and explicit persistence
  scope badges across runtime vs. persisted actions.
- **Server hardening and documentation showcase.** Hardened overlay promotion, TLS/DNS validation guards, stats admin
  routing, dynamic OpenAPI contract versioning, refreshed embedded stats page assets, and live TUI/Web UI showcase GIFs
  in `README.md`.

## Configuration engine & 7-format parity

- **Preserved runtime tuning during draft composition.** `oxiroute-config` preserves all runtime tuning fields across
  draft composition, overlay application, and round-trip serialization.
- **Unified format registry and jailed include loader.** `oxiroute-config-source` consolidates format detection,
  decoding, rendering, and directory-jailed, symlink-safe multi-file loading with an integration parity suite
  (`parity_and_overrides`) registered in the core CI tier.

## Management API & CLI parity

- **Multi-file workspace API.** Added `/api/v1/config/files` (list workspace graph and dependencies) and
  `/api/v1/config/files/{path}` (`GET`, `POST /validate`, `PUT` with `If-Config-Revision` conflict detection), plus
  `/api/v1/config/convert` and `/api/v1/config/patch`.
- **CLI multi-file editing, format conversion, and resource CRUD.** Added `oxiroute config files`, multi-file
  `oxiroute config edit --file <relpath>`, `oxiroute config convert`, and declarative `oxiroute pool|listener|route|site`
  subcommands sharing the same patch/validation pipeline as the API.
- **Server hardening.** Hardened overlay promotion semantics, TLS and DNS guards, stats admin endpoint routing, and
  derived OpenAPI contract versions directly from `CARGO_PKG_VERSION`.

## Management Web UI & Stats Page

- **Unified 7-format UI decoder registry & structured editors.** Replaced ad-hoc format handling in the UI with a single
  7-format registry and completed structured editors across all configuration sections (aligning RTMP service fields with
  the canonical `RtmpService` schema).
- **Source & includes, History & rollback, Overlays, and Route debugger.** Added the multi-file **Source & includes**
  editor with whole-graph validation, **History & rollback** with `Diff vs current`, **Operations → Overlays** with
  one-click clear/promote, and **Operations → Route debugger** backed by `/api/v1/route/explain`, along with clear
  persistence indicators (runtime-only vs. `.overlay.json` vs. config file).
- **Embedded stats page refresh.** Rebuilt and synchronized the embedded pure-Vapor stats page assets with the latest UI
  build.
- **README visual showcase.** Added live-traffic animated GIFs of `oxiroute top` (`docs/assets/oxiroute-top.gif`) and
  the Management Web UI & Stats Page (`docs/assets/oxiroute-webui.gif`).

## Performance

7.3.0 changes no data-plane hot path, so the 7.1 benchmark figures in [the benchmarks](BENCHMARKS.md) and the
[7.1 report](../benchmarks/reports/7.1/README.md) stay and were not repeated.

## Upgrading

Nothing in an existing configuration file needs editing to upgrade from 7.2.0.

- All new API endpoints (`/api/v1/config/files*`, `/api/v1/config/convert`, `/api/v1/config/patch`), CLI subcommands,
  and Web UI tabs are additive.
- Existing single-file and compositional `.oxi`/KDL/YAML/TOML/Lua/HOCON/UCI configurations continue to load and reload
  unchanged.

## Build, packaging and CI

- Release assets follow the 7.2.0 set at version `7.3.0`: deterministic source archive and provenance, profile-guided
  x86_64 (`unwind` and `abort`) and aarch64 (`unwind`) tarballs built on glibc 2.34, `.deb` (`amd64`, `arm64`),
  `.el9.rpm` and `.amzn2023.rpm` (`x86_64`, `aarch64`), the Arch package (`oxiroute-7.3.0-1-x86_64.pkg.tar.zst` and its
  `.sha256`), per-asset `.build.json` records, and one `SHA256SUMS`.

## Install

Every release attaches its assets to the [GitHub release](https://github.com/brauliobo/oxiroute/releases). There is no
hosted apt or yum repository: download the file you need and `SHA256SUMS` into one directory, then verify and install.

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <asset> --repo brauliobo/oxiroute          # optional

sudo apt install ./oxiroute_7.3.0_amd64.deb                      # Debian 12/13, Ubuntu 22.04/24.04 (arm64: _arm64.deb)
sudo dnf install ./oxiroute-7.3.0-1.el9.x86_64.rpm               # AlmaLinux and Rocky Linux 9 (aarch64: .el9.aarch64.rpm)
sudo dnf install ./oxiroute-7.3.0-1.amzn2023.x86_64.rpm          # Amazon Linux 2023 (aarch64: .amzn2023.aarch64.rpm)
sudo pacman -U ./oxiroute-7.3.0-1-x86_64.pkg.tar.zst             # Arch Linux (x86_64 only; AUR: oxiroute-bin)

tar -xzf oxiroute-7.3.0-x86_64-unknown-linux-gnu.tar.gz          # any glibc 2.34 or newer Linux
sudo install -m 0755 oxiroute-7.3.0-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
```

[Install a release package](user/INSTALL.md) documents the service opt-in and upgrade, remove, and purge behaviour. The
Arch packages (`oxiroute`, `oxiroute-bin`) are described in the [Arch packaging guide](../packaging/arch/README.md).

## Known limits

- **TOML `null`.** TOML has no `null` value, so rendering a configuration containing explicit nulls to TOML is rejected;
  omit optional fields or render to `.oxi`, KDL, YAML, HOCON, or Lua instead.
- **Earlier 7.2.x / 7.1.x limits** (memory-only Lua `include`/`read_*`, tracking scope, `top` views, `oxiroute upgrade`,
  reload state, FLV recordings, RTMP exec profiles, event ring) are unchanged; see
  [the 7.2.0 notes](RELEASE_NOTES_7.2.0.md#known-limits) and the [compatibility matrix](COMPATIBILITY.md).
