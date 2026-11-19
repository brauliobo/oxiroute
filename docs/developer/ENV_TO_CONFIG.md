# Environment variables and the configuration

Every tunable behaviour or setting of the daemon is a configuration option, available in every
source format (KDL, restricted Lua, UCI, HOCON), the UI field registry, `coverage/canonical.json`,
validation and `docs/CONFIG_SPEC.md`. An environment variable is kept only where the configuration
cannot hold the value, or as a documented fallback that the configuration overrides. This note is
the inventory of every variable the product reads, with its decision.

Classes: (a) tunable setting, now a configuration option; (b) bootstrap or process plumbing,
third-party convention, or test-only hook that stays outside the configuration; (c) secret source,
kept out of the configuration values but nameable by a path option.

Precedence for a setting that has both: the configuration key, then the environment variable (the
rule the reload drain has followed since 6.0), then the default. The fallbacks log no warning.

## Variables read by the product (`crates/`, `vendor/`)

| Variable | Read in | Purpose | Class | Decision |
| --- | --- | --- | --- | --- |
| `OXIROUTE_MANAGEMENT_TOKEN_FILE` | daemon: `rtmp::api::management_token_file` (used by `main.rs` and `generation/prepared.rs`); client: `cli/commands.rs`; forwarded to workers by `supervised/master/runner.rs` | Path of the management bearer token file | (c) | New `management.token_file`. The daemon variable stays as the documented fallback; a configured listener still needs one of them. The CLI client reads its own path from `--token-file`, this variable, the package env file or the default path, because it has no access to the daemon configuration. |
| `OXIROUTE_AUDIT_DIR` | `observability/operational_event/event_log.rs` | Directory of the durable audit log | (a) | New `audit.dir`. Variable kept as a deprecated fallback. |
| `OXIROUTE_AUDIT_MAX_RECORDS` | `operational_event/audit.rs` | Records retained | (a) | New `audit.max_records`. Variable kept as a deprecated fallback. |
| `OXIROUTE_AUDIT_MAX_RECORD_BYTES` | same | Largest record | (a) | New `audit.max_record_bytes`. Fallback kept. |
| `OXIROUTE_AUDIT_MAX_FILE_BYTES` | same | Rotation size | (a) | New `audit.max_file_bytes`. Fallback kept. |
| `OXIROUTE_AUDIT_MAX_TOTAL_BYTES` | same | Bytes retained overall | (a) | New `audit.max_total_bytes`. Fallback kept. |
| `OXIROUTE_AUDIT_MAX_ROTATED_FILES` | same | Rotated files kept | (a) | New `audit.max_rotated_files`. Fallback kept. |
| `OXIROUTE_RELOAD_DRAIN_SECONDS` | `supervised/master/setup.rs`, `supervised/worker.rs` | Drain window of a replaced worker | (a) | Already `retirement.drain_deadline_ms` since 6.0; the variable stays as the deprecated fallback. No change. |
| `RUST_LOG` | `observability/log_filter.rs` (was `env_logger::init` in `main.rs` and `supervised/worker.rs`) | Log verbosity | (a) | New root `log_filter` in `RUST_LOG` syntax without regex filters. The variable stays the fallback. Supervised workers run with a cleared environment; the master forwards `RUST_LOG` to them, so they and the master share the fallback, and they honour `log_filter`. |
| `OXIROUTE_CPU_LEVEL` | `oxiroute-cpu/src/level.rs` (`init` at process start, `level` on first use), forwarded to workers by `supervised/master/runner.rs` | Instruction-set level of the byte scanners | (a) | New root `cpu_level`. The variable stays the fallback: `scalar`, `sse2`, `sse42`, `avx2` or `avx512`, capped to the CPU. `main` validates it at start and refuses an invalid value with an error naming the variable; supervised workers apply their generation's `cpu_level` before any dispatch. |
| `OXIROUTE_ENDPOINT` | `cli.rs` (clap `env`) | Management endpoint the CLI client talks to | (b) | Client-side address; the client does not read the daemon configuration. Stays, with `--endpoint`. |
| `OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME` | `supervised/master.rs` | Forces the direct runtime in process tests | (b) | Test-only hook. Stays. |
| `OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE` | `supervised/worker.rs` | Injects a worker runtime failure | (b) | Test-only hook. Stays. |
| `OXIROUTE_INTERNAL_TEST_LISTENER_DUPLICATION_FAILURE` | `main/listener_services.rs` | Injects a listener descriptor failure | (b) | Test-only hook. Stays. |
| `INVOCATION_ID`, `NOTIFY_SOCKET` | `supervised/master/handover/notify.rs` | Whether the master runs as a systemd service, and the socket on which it tells systemd that the successor of a binary upgrade is the main process | (b) | systemd's own convention; there is no configuration to hold it. A service without `NotifyAccess=` has no `NOTIFY_SOCKET`, and the upgrade refuses then. |
| `PATH` | `oxiroute-supervisor-process/src/spawn.rs` | Resolves a bare executable name | (b) | Operating-system convention. Stays. |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | read by `rustls-native-certs` (`http3/upstream_plan.rs`, `vendor/pingora-rustls`) | Where the platform trust store lives | (b) | Platform convention of the TLS library. An explicit CA bundle is already a per-pool configuration option (`ca_file`). Stays; forwarded to supervised workers, which read the platform trust store like the master. |
| `SSLKEYLOGFILE` | `vendor/pingora-core/src/connectors/tls/boringssl_openssl/mod.rs` (and rustls `KeyLogFile`) | TLS key log for debugging | (b) | Reachable only when `debug_ssl_keylog` is set, which the daemon never does. Unreachable in the product; stays. |
| `CARGO_PKG_VERSION`, `CARGO_MANIFEST_DIR`, `OXIROUTE_SERVER_MANIFEST_DIR` | `env!`/`option_env!` in `cli.rs`, `*/contract.rs`, tests | Compile-time values | (b) | Build time, not runtime. |
| `OXIROUTE_CONFIG_SAVE_*` | `config/coordinator/tests.rs` | Child process of a test | (b) | Test only. |

The worker command (`supervised/master/runner.rs`) clears the environment and forwards only
`OXIROUTE_MANAGEMENT_TOKEN_FILE`, the `OXIROUTE_AUDIT_*` fallbacks, `RUST_LOG`, `OXIROUTE_CPU_LEVEL` and the
third-party conventions `_RJEM_MALLOC_CONF`, `SSL_CERT_FILE` and `SSL_CERT_DIR`. A configured value travels
inside the launch document and needs no forwarding. RTMP exec profiles clear the environment and
declare their variables in the configuration already.

## Files that set variables

| File | Variables | Decision |
| --- | --- | --- |
| `packaging/arch/oxiroute.env` (and `bin/`) | `RUST_LOG`, `OXIROUTE_AUDIT_*`, commented `OXIROUTE_MANAGEMENT_TOKEN_FILE` and `OXIROUTE_RELOAD_DRAIN_SECONDS` | Still valid fallbacks, so installed conffiles keep working. The package files (`oxiroute.env`, `oxiroute.kdl`, `README.md`) are checksum-pinned and byte-identical in `bin/`, so moving the defaults into the packaged configuration and updating the README wording is a packaging change for the release that re-pins them. |
| `packaging/arch/oxiroute.service` | `EnvironmentFile=-/etc/oxiroute/oxiroute.env` only | No variable of its own. |
| `packaging/arch/pin-bin-release.sh` | `OXIROUTE_RELEASE_URL`, `OXIROUTE_RAW_URL` | Maintainer tooling (b). |
| `scripts/` | `OXIROUTE_ACME_STAGING_*`, `OXIROUTE_API_TARGET`, `OXIROUTE_API_TOOLCHAIN`, `OXIROUTE_GENERATION_BASELINE_*`, `OXIROUTE_BIN` | Test and release tooling, never read by the daemon (b). |

## Variables of the 6.4 performance work

| Variable | Decision |
| --- | --- |
| `OXIROUTE_TLS_HANDSHAKE_THREADS`, `OXIROUTE_HTTP_LANE`, `OXIROUTE_TEST_BIN`, `OXIROUTE_PROTO_SPLICE` | Research and branch switches that no longer exist: the settings are the root options `tls_handshake_threads` and `http_fast_lane`; the splice and sendfile paths have no switch. |
| `OXIROUTE_INTERNAL_TEST_AGED_PER_WORKER` | Test-only hook of the scheduler tests; the setting is the root option `connection_scheduling`. Stays (b). |
| `_RJEM_MALLOC_CONF` | jemalloc's run-time convention; overrides the allocator configuration embedded in the binary (`crates/oxiroute-allocator`). Stays (b) as a startup-only environment variable: it configures the allocator of the process before `main`, a new worker inherits the master's value (forwarded by `supervised/master/runner.rs`), so it changes only with a master restart or a binary hand-over. |
| `OXIROUTE_INTERNAL_TEST_LISTENER_PANIC` | Test-only hook that makes a listener service panic inside a real worker. Stays (b). |
| `INSTR_PERIOD`, `DIFF_PRINT`, `ROBUSTNESS_SCALE` | Benchmark and test knobs of `oxiroute-h2`, not product reads. |

## New options

| Option | Default | Range | Reload |
| --- | --- | --- | --- |
| `management.token_file` | none (falls back to the variable) | absolute normalized path | read when a generation starts |
| `audit.dir` | `audit` next to the token file, else `OXIROUTE_AUDIT_DIR`, else in memory | absolute directory path | read when a generation starts |
| `audit.max_records` | 10000 | 1 to 100000 | same |
| `audit.max_record_bytes` | 16384 | 1 to 65536, at most `max_file_bytes` | same |
| `audit.max_file_bytes` | 1 MiB | 1 to 16 MiB | same |
| `audit.max_total_bytes` | 8 MiB | 1 to 128 MiB, at least `max_file_bytes` | same |
| `audit.max_rotated_files` | 7 | 1 to 7 | same |
| `config_history.max_entries` | 20 | 1 to 1000 | read at each configuration save |
| `config_history.max_total_bytes` | 16 MiB | 4 MiB to 1 GiB | same |
| `log_filter` | none (falls back to `RUST_LOG`, then errors only) | `RUST_LOG` syntax without regex, at most 1024 bytes | master: start and each committed replacement; worker: start; direct runtime: each generation |
| `cpu_level` | none (falls back to `OXIROUTE_CPU_LEVEL`, then the CPU's level) | `scalar`, `sse2`, `sse42`, `avx2` or `avx512`, lowered to the CPU | master: start; worker: start, so the next reload that starts a worker applies it; direct runtime: process start only, a change is reported as `cpu_level` waiting for a restart |

Out-of-range values that the `OXIROUTE_AUDIT_*` variables used to clamp are rejected in the
configuration, naming the setting. An empty `audit` block normalizes away.

## Re-checking the inventory

```sh
rg -n 'env::var(_os)?\(|env!\(|option_env!\(|env *= *"|\.env\(' crates vendor --glob '*.rs' \
  --glob '!**/tests/**' --glob '!**/tests.rs'
```

A new hit is a runtime setting unless it is one of the rows above; add the configuration option
instead of a variable, or record why it cannot live in the configuration.
