# OxiRoute 6.5.0

Unreleased.

## Connections

- **A new connection costs about 1.2k fewer user-space instructions (-3.5%, 34.6k to 33.4k for a proxied 1 KiB request on a connection per
  request)** and 12 kB less copying: the connection future is boxed once where it is spawned, the layers of an HTTP listener hand
  each other their futures instead of a box each, and the accept loop is one loop for every listener. Behaviour, limits, drain
  and socket options are unchanged.
- No scheduling default changed. Measured against nginx and HAProxy on one worker (`docs/developer/PERF_NEXT_CONNECTIONS_RESULTS.md`):
  `connection_scheduling = "aged_per_worker"` serves 1.2x to 1.5x of nginx's rate at 1,024 and 2,048 connections with a p99 of 1.6x and 1.4x
  nginx's (work stealing: 3.3x to 5.5x), at 0.7x of work stealing's rate at 1,024 connections on one worker (1.0x at 2,048 and at 256);
  from two workers up the two are within noise. It stays opt-in (and `auto` selects it from two workers).

## Benchmarks

- `benchmarks/scripts/vs_nginx_haproxy.py`: the connection-count scenarios cover 64, 256, 512, 1,024 and 2,048 connections and HAProxy;
  `VS_PERF_EXTRA` adds counters such as `cache-misses:u` per request to every run; `VS_ALT_BINS` builds can carry their own environment
  (`label=/path@KEY=VALUE`); runs disturbed on the SMT siblings of the proxy CPUs are discarded (`--sibling-max`).
