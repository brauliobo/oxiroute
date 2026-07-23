# OxiRoute local-v1 benchmarks

`local-v1` is a deliberately narrow localhost benchmark for plaintext reverse HTTP/1.1. It compares
OxiRoute, nginx, and HAProxy as single-worker reverse proxies in front of the same dedicated nginx
origin. It does not claim production sizing, network scalability, cache performance, or protocol
coverage.

The direct harness is authoritative. The checked-in Phoronix profile invokes that harness and only
parses its requests-per-second result; it does not upload profiles or results.

## Scope

- One Linux host, loopback TCP, HTTP/1.1 keep-alive, and a fixed 1 KiB origin response.
- One proxy process at a time on `127.0.0.1:19081` and one nginx origin on
  `127.0.0.1:19080`.
- One OxiRoute/Pingora service thread, one nginx worker, and one HAProxy thread.
- ApacheBench warm-up followed by a measured run. Raw output, parsed JSON, logs, rendered configuration,
  preflight data, and environment data are retained together.
- Origin, proxy, and load generator are pinned to distinct configured CPUs. The harness performs no
  governor/kernel tuning and does not cover TLS, h2c, cache warming, remote clients, or uploads.

`lanes.json` is the machine-readable lane contract. Cache, forward-proxy, H2, and H3 lanes are
explicit skips. OxiRoute's current configuration schema describes cache and forward-proxy objects,
but the daemon does not activate cache and its runtime planner rejects forward-proxy listeners.
Reverse H2 requires TLS and a different client/configuration matrix; reverse H3 is not implemented.
Those results must not be inferred from reverse H1.

## Requirements

- Linux with `/proc`, Bash 4 or newer, Python 3, ApacheBench, nginx, and HAProxy.
- A release OxiRoute binary at `target/release/oxiroute-server`, or an executable path in
  `OXIROUTE_BIN`.
- The benchmark ports must be free. The preflight checks this before any process starts.

Build OxiRoute outside the harness so compilation is never mixed with measurement:

```sh
cargo build --release -p oxiroute-server
```

Run preflight and capture the host environment without starting services:

```sh
benchmarks/scripts/preflight.sh
benchmarks/scripts/environment.sh
```

Run all reverse-H1 implementations:

```sh
benchmarks/scripts/run.sh
```

Run one implementation, useful for controlled repetition:

```sh
benchmarks/scripts/run.sh --implementation oxiroute
benchmarks/scripts/run.sh --implementation nginx
benchmarks/scripts/run.sh --implementation haproxy
```

The following environment variables tune a run without changing checked-in configuration:

| Variable | Default | Meaning |
| --- | ---: | --- |
| `OXIROUTE_BIN` | `target/release/oxiroute-server` | Release daemon path |
| `BENCH_ORIGIN_PORT` | `19080` | Dedicated origin port |
| `BENCH_PROXY_PORT` | `19081` | Sequential proxy port |
| `BENCH_CONNECTIONS` | `128` | ApacheBench concurrency |
| `BENCH_WARMUP_SECONDS` | `10` | Warm-up duration |
| `BENCH_DURATION_SECONDS` | `30` | Measured duration |
| `BENCH_STOP_TIMEOUT_SECONDS` | `10` | Graceful child-stop deadline |
| `BENCH_PROXY_CPU` | `2` | CPU assigned to the proxy process |
| `BENCH_ORIGIN_CPU` | `3` | CPU assigned to the origin process |
| `BENCH_LOAD_CPU` | `4` | CPU assigned to ApacheBench |

Every invocation creates an ignored directory under `benchmarks/generated/runs/`. A successful
implementation has `summary-<implementation>.json` and `ab-<implementation>.txt`. `skips.json`
contains the unavailable lane records copied from `lanes.json`, and `run.json` records the effective
ports, durations, threads, and connections. A failed run remains on disk for diagnosis.

## Configuration validation

`scripts/validate.sh` renders all templates and performs non-starting checks:

- unresolved placeholder detection for every template;
- `luac -p` when `luac` is installed;
- `nginx -t` for the origin and reverse-proxy configurations;
- `haproxy -c` for the HAProxy configuration;
- XML parsing for the Phoronix profile.

OxiRoute currently has no validate-only CLI. Its benchmark template follows the checked-in v1 Rust
schema, but full runtime preparation is first exercised when `run.sh` starts the daemon.

## Phoronix wrapper

The profile is checked in at `phoronix/local/oxiroute-local-v1`. The wrapper uses an isolated
`PTS_USER_PATH` below `benchmarks/generated/phoronix`, links the local profile into that private
tree, and invokes only local Phoronix install/benchmark commands:

```sh
benchmarks/scripts/run-phoronix.sh
```

Phoronix presents OxiRoute, nginx, and HAProxy as test options and repeats the direct single-
implementation harness three times. The wrapper does not invoke any OpenBenchmarking upload,
login, refresh, or network command. Keep the isolated result directory if a local Phoronix report
is needed.

## Process safety

All service processes are direct children of the harness; nginx master mode is disabled to avoid an
untracked worker child. `scripts/lib.sh` records the PID and Linux process start-time field
immediately after launch. Cleanup checks both values, sends `TERM`, waits, and only then sends
`KILL` to that same identity if necessary. It never uses process-name matching,
`killall`, `pkill`, `systemctl`, or distribution service state.
