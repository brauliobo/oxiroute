# OxiRoute 7.2.0

Prepared 2026-11-24 for the `v7.2.0` tag.

OxiRoute 7.2.0 adds the UCI-inspired `.oxi` stanza format (with inline lists, embedded sandboxed Lua 5.4, and
hot-reloaded multi-file `include`), strict YAML 1.2 and TOML formats, cross-format `vars`/parameterized templates and
high-level `edge`/`sites` configuration sugar across all 7 formats, expanded sandboxed Lua 5.4 scripting with a built-in
helper library and `oxi` builder DSL, nginx/HAProxy CLI parity upgrades (`config check --no-probe/--dump`, `config test`,
`version -v`, top-level `reload`), and automatic `/etc/oxiroute/management.token` creation on package install and upgrade
across Arch, Debian/Ubuntu, and RPM packages. Existing configuration files need no edits to upgrade.

## Headlines

- **`.oxi` stanza configuration format.** Flat, two-space-indented stanzas (`site`, `template`, `pool`, `cert`, `tls`,
  `service`, `listener`, `cache`, `forward_proxy`, `rtmp`, `l4`, native `*_server`) with inline space-separated lists,
  arrow route shorthands (`http ... -> https`, `route /api -> 127.0.0.1:5000`), sandboxed multi-file `include`
  (`.oxi`/`.yaml`/`.toml`/`.json`/`.lua` with hot-reload dependency tracking), embedded sandboxed Lua 5.4 (`lua` blocks
  and `lua(...)` expressions), and deterministic lossless rendering.
- **Cross-format parameterized templates, `vars`, and `edge`/`sites` sugar.** Root `vars`, template `params` defaults,
  parameterized `use`, typed/string `{{var}}` interpolation, and high-level `edge`/`sites` desugaring work identically
  across all 7 formats (`.oxi`, KDL, YAML, TOML, Lua, HOCON, UCI) and coexist cleanly with low-level `ConfigDraft`
  sections.
- **YAML 1.2 and TOML configuration formats.** `.yaml`/`.yml` and `.toml` join KDL 2.0, `.oxi`, Lua, HOCON, and OpenWrt
  UCI across `serve`, `init`, `config check`, `config compose`, and the management API, verified by an exhaustive
  7-format full-schema round-trip coverage suite.
- **Sandboxed Lua 5.4 scripting, `oxi` DSL, and modular loaders.** `.lua` configs run in a bounded, deterministic Lua
  5.4 sandbox with collection/string/encoding/hashing/network helpers, concise `oxi.*` builders, and directory-jailed
  `include`/`read_*` helpers whose files are automatically watched for hot reload.
- **Nginx & HAProxy CLI parity upgrades.** `oxiroute config check` (aliased as `config test`) gains `--no-probe`
  (offline check without port binding) and `--dump [--format <FMT>]` (`nginx -T` in any format), `oxiroute version -v`
  prints detailed build/capability info (`nginx -V` / `haproxy -vv`), top-level `oxiroute reload [--wait]` matches
  `nginx -s reload`, and packaged config discovery checks `/etc/oxiroute/oxiroute.{oxi,kdl,yaml,yml,toml,lua,conf}`.
- **Automatic management token creation on package install and upgrade.** Every package family (Arch, Debian/Ubuntu,
  RPM) runs a shared helper (`/usr/lib/oxiroute/ensure-management-token`) to generate `/etc/oxiroute/management.token`
  (`0600 oxiroute:oxiroute`) when missing, so `oxiroute` CLI commands and a configured management listener work out of
  the box without manual token setup.

## `.oxi` stanza format, cross-format templates, and `edge`/`sites` sugar

The full `.oxi` syntax and sugar reference is in [OXI configuration format](OXI_FORMAT.md) and
[Configuration formats](CONFIG_FORMATS.md).

- **Minimalist stanza syntax (`.oxi`).** Two-space-indented flat stanzas without braces, brackets, commas, or quotes on
  bare paths, hosts, IPs, URLs, or globs, plus explicit `[item]`/`[]`/`{}` containers and `- ` anonymous list items for
  lossless round-trip rendering of any `ValidatedConfig`.
- **Embedded Lua 5.4 and multi-file `include`.** `.oxi` documents support top-level `lua` blocks (with `vars` exposed as
  a global), key-level `lua` blocks, inline `lua(...)` expressions, and sandboxed relative `include` across `.oxi`,
  `.yaml`, `.toml`, `.json`, and `.lua` files with automatic hot-reload tracking via `ResolvedSource.dependencies`.
- **Cross-format `vars` and parameterized templates.** Every format now supports root `vars`, template `params` defaults,
  parameterized `use` arguments, whole-value typed `{{var}}` and embedded `"127.0.0.1:{{port}}"` interpolation, dotted
  lookups, `\{{` escaping, and `MAX_SUBSTITUTIONS` / `MAX_EXPANSION_DEPTH` / `MAX_OUTPUT_BYTES` bounds.
- **Cross-format `edge` and `sites` sugar.** High-level `edge` (`http`, `https`, `redirect_https`, `stats`, `management`)
  and `sites` (`host`, `cert` for Certbot/self-signed/files/ACME/reuse, `proxy`, `static`, `redirect`, `respond`,
  sub-path `routes`, `gzip`) desugar deterministically into canonical `ConfigDraft` sections in every format.
- **Exhaustive 7-format coverage.** `tests/format_coverage.rs` verifies full kitchen-sink `ValidatedConfig` round-trips,
  identical `vars`/parameterized template resolution, identical `edge`/`sites` sugar resolution, TOML fail-closed
  explicit-`null` rejection, and shared resource bounds across all 7 formats.

## Nginx & HAProxy CLI parity

- **`oxiroute config check` / `config test`.** Added `--no-probe` to validate and prepare configurations offline without
  attempting to bind listening ports, and `--dump [--format <FMT>]` to print the fully resolved effective configuration
  in any supported format.
- **`oxiroute version -v / --verbose`.** Prints version, target arch/OS, supported config formats, protocols, TLS
  backends, data planes, and detected CPU SIMD level (human table or `--output json`).
- **Top-level `oxiroute reload [--wait]`.** Mirrors `oxiroute generation reload` alongside top-level `drain` and
  `shutdown`.
- **Multi-format packaged discovery.** The CLI automatically discovers management endpoints and token files from
  `/etc/oxiroute/oxiroute.{oxi,kdl,yaml,yml,toml,lua,conf}`.

## YAML 1.2 and TOML configuration formats

The full format contract is in [Configuration formats](CONFIG_FORMATS.md).

- **Strict YAML 1.2 (`.yaml`, `.yml`).** Bounded decoder and deterministic minimalist renderer with sorted object keys
  and preserved array order. Rejects anchors, aliases, tags, multi-document streams, duplicate mapping keys, and
  non-finite floats so documents remain self-contained and unambiguous.
- **Bounded TOML (`.toml`).** Bounded decoder and deterministic renderer with duplicate-key, depth, node, string, and
  size limits. TOML has no `null` type and rejects TOML datetime values where the canonical schema expects strings or
  numbers.
- **Unified pipeline, CLI, and management API.** Both formats infer cleanly from file extensions, participate in generic
  `templates`/`use` expansion and top-level native server references (`nginx_server`, `haproxy_server`, `squid_server`,
  `apache_server`, `varnish_server`), and are selectable in `oxiroute init --format yaml|toml`, `oxiroute config compose
  --format yaml|toml`, and the management configuration source API.

## Sandboxed Lua 5.4 scripting

The complete scripting guide and function reference is in [Lua 5.4 Scripting](LUA_SCRIPTING.md).

- **Bounded, deterministic sandbox.** Each load runs in a fresh Lua 5.4 VM capped at 4 MiB heap and 1,000,000
  instructions, rejecting binary bytecode chunks and removing `os`, `io`, `package`, `debug`, `coroutine`, `dofile`,
  `loadfile`, `load`, `require`, `math.random`, `math.randomseed`, and metatable/raw mutation escapes.
- **Unified pipeline.** Lua scripts participate in the same generic `templates`/`use` expansion and native server
  reference resolution as every other format, with explicit `null`, `array(tbl)`, and `object(tbl)` value-shaping
  helpers.
- **Built-in deterministic helpers.** Registers collection/functional helpers (`clone`, `merge`, `deep_merge`, `keys`,
  `values`, `map`, `filter`, `flat_map`, `concat`, `unique`, `contains`, `slice`, `range`, `group_by`), `str.*` string
  helpers, `json.*`/`yaml.*`/`toml.*`/`base64.*` codecs, `hash.sha256`, and `net.*` IP/CIDR/hostname helpers.
- **`oxi` / `oxiroute` builder DSL.** Concise table constructors (`oxi.config`, `oxi.certbot`, `oxi.cert_files`,
  `oxi.cert_self_signed`, `oxi.tls_profile`, `oxi.upstream`, `oxi.host`, `oxi.path`, `oxi.proxy`, `oxi.redirect`,
  `oxi.fixed_response`, `oxi.static_files`, `oxi.route`, `oxi.http_service`, `oxi.listener`) that return plain Lua
  tables matching `ConfigDraft`.
- **Modular configs and external data loading.** On-disk evaluation exposes directory-jailed, no-symlink `include`,
  `read_text`, `read_lines`, `read_json`, `read_yaml`, and `read_toml` helpers with cycle, depth, and size bounds. Every
  loaded file is recorded in `ResolvedSource.dependencies`, so editing an included module or external data file triggers
  automatic hot reload.

## Packaging and management token

- **Autogenerated management token.** Arch, Debian/Ubuntu, and RPM packages install
  `/usr/lib/oxiroute/ensure-management-token` and call it on install and upgrade. When `/etc/oxiroute/management.token`
  does not exist (and is not a symlink), it ensures the `oxiroute` system user exists, creates a 64-character random hex
  token with mode `0600` owned by `oxiroute:oxiroute`, and removes the file if ownership assignment fails. Existing files
  are never replaced, and reinstalls keep the token unchanged.
- **Packaged environment default.** `/etc/oxiroute/oxiroute.env` sets
  `OXIROUTE_MANAGEMENT_TOKEN_FILE=/etc/oxiroute/management.token` so both the systemd service and the CLI discover it
  automatically.
- **Removal and purge.** Debian `apt remove` keeps `/etc/oxiroute/management.token` and `apt purge` removes it along
  with `/etc/oxiroute`; RPM lists it as `%ghost %attr(0600,oxiroute,oxiroute)` so `dnf remove` / `rpm -e` deletes it.
- **Shared staged installation.** The deb, rpm, and release Arch recipes share `packaging/install-staged.sh` and
  `packaging/arch/oxiroute.ensure-token`, verified by the offline packaging selftest and container install suites.

## Fixed

- **Reverse HTTP/3 test stability.** The control-stream reset test no longer depends on packet timing.
- **AUR publish test.** `packaging/arch/test/publish-aur.sh` no longer depends on whether `bin/PKGBUILD` is already
  pinned.

## Performance

7.2.0 changes no data-plane hot path, so the 7.1 benchmark figures in [the benchmarks](BENCHMARKS.md) and the
[7.1 report](../benchmarks/reports/7.1/README.md) stay and were not repeated.

## Upgrading

Nothing in an existing configuration file needs editing to upgrade from 7.1.1.

1. **Management token on upgrade.** Upgrading a package on Arch, Debian/Ubuntu, or RPM creates
   `/etc/oxiroute/management.token` (`0600 oxiroute:oxiroute`) if it is missing and leaves any existing token file
   untouched.
2. **Lua configurations with `include` or `read_*`.** Scripts that load external files are marked `compositional` so
   typed API saves do not overwrite the authored script (`E_COMPOSITIONAL_ROOT`); authored-text saves (`PUT
   /api/v1/config/source`, `oxiroute config apply|edit`) continue to preserve the Lua source verbatim.

## Build, packaging and CI

- Release assets follow the 7.1.1 set at version `7.2.0`: deterministic source archive and provenance, profile-guided
  x86_64 (`unwind` and `abort`) and aarch64 (`unwind`) tarballs built on glibc 2.34, `.deb` (`amd64`, `arm64`),
  `.el9.rpm` and `.amzn2023.rpm` (`x86_64`, `aarch64`), the Arch package (`oxiroute-7.2.0-1-x86_64.pkg.tar.zst` and its
  `.sha256`), per-asset `.build.json` records, and one `SHA256SUMS`.
- The container test suite verifies `/usr/lib/oxiroute/ensure-management-token`, `/etc/oxiroute/management.token` mode,
  ownership, size, and readability by `oxiroute`, and that reinstall preserves the token.

## Install

Every release attaches its assets to the [GitHub release](https://github.com/brauliobo/oxiroute/releases). There is no
hosted apt or yum repository: download the file you need and `SHA256SUMS` into one directory, then verify and install.

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify <asset> --repo brauliobo/oxiroute          # optional

sudo apt install ./oxiroute_7.2.0_amd64.deb                      # Debian 12/13, Ubuntu 22.04/24.04 (arm64: _arm64.deb)
sudo dnf install ./oxiroute-7.2.0-1.el9.x86_64.rpm               # AlmaLinux and Rocky Linux 9 (aarch64: .el9.aarch64.rpm)
sudo dnf install ./oxiroute-7.2.0-1.amzn2023.x86_64.rpm          # Amazon Linux 2023 (aarch64: .amzn2023.aarch64.rpm)
sudo pacman -U ./oxiroute-7.2.0-1-x86_64.pkg.tar.zst             # Arch Linux (x86_64 only; AUR: oxiroute-bin)

tar -xzf oxiroute-7.2.0-x86_64-unknown-linux-gnu.tar.gz          # any glibc 2.34 or newer Linux
sudo install -m 0755 oxiroute-7.2.0-x86_64-unknown-linux-gnu/bin/oxiroute /usr/local/bin/oxiroute
```

[Install a release package](user/INSTALL.md) documents the service opt-in and upgrade, remove, and purge behaviour. The
Arch packages (`oxiroute`, `oxiroute-bin`) are described in the [Arch packaging guide](../packaging/arch/README.md).

## Known limits

- **TOML `null`.** TOML has no `null` value, so rendering a configuration containing explicit nulls to TOML is rejected;
  omit optional fields or render to KDL, YAML, HOCON, or Lua instead.
- **Memory-only Lua evaluation.** `include` and `read_*` are available only when resolving from disk; memory-only entry
  points (`decode_value`, `load_lua`) disable filesystem helpers.
- **Earlier 7.1.x limits** (tracking scope, `top` views, `oxiroute upgrade`, reload state, FLV recordings, RTMP exec
  profiles, overlay promotion, event ring) are unchanged; see [the 7.1.1 notes](RELEASE_NOTES_7.1.1.md#known-limits) and
  the [compatibility matrix](COMPATIBILITY.md).
