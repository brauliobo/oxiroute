# OxiRoute 6.5.0 (Unreleased)

## Memory

- An idle plain WebSocket connection costs 16.6 kB of resident memory instead of 31.6 kB (nginx 18.4 kB, HAProxy 6.3 kB), an
  idle WebSocket over TLS 38.6 kB instead of 53.5 kB (nginx 60.1 kB, HAProxy 30.9 kB) and an idle HTTP/1 keep-alive
  connection 11.9 kB instead of 17.8 kB (nginx 9.2 kB, HAProxy 11.2 kB). An upgraded connection no longer keeps the
  request's state machine: the handshake ends the request, the connection continues as a small tunnel and the hooks and
  the access log run when it ends.
- A close-delimited HTTP/1 body (and so an upgraded connection) no longer allocates its 128 KiB chunk buffer before it
  reads.
- jemalloc returns freed pages from a background thread (`background_thread:true`). After connection bursts and reloads
  the process settles at about half of the resident memory it used to keep (97.9 MB to 48.8 MB in the compressed churn of
  `benchmarks/r65-memory/churn.py`). `_RJEM_MALLOC_CONF` still overrides the setting.
- A connection's task and request context are smaller (the connection counter wraps the future without copying it,
  the cache state of a request is boxed until a cacheable request needs it).
- Instructions per request on the fast lane, HTTP/1 1 MiB, HTTP/1 over TLS and HTTP/2 over TLS are within +0.7% of 6.4.0.

## Tools

- `benchmarks/r65-memory/` (churn, time to first byte of a parked connection, live libc allocation tracer) and
  `benchmarks/scripts/memsample.py` measure the memory of a proxy including its kernel socket memory;
  `benchmarks/scripts/vs_nginx_haproxy.py` has the scenarios `idle_h1`, `idle_h1tls` and `idle_h2tls` next to `idle_ws` and
  `idle_wstls` and reports anonymous memory, socket buffers and kernel socket structures per connection.
- The `heap-profile` cargo feature of `oxiroute` builds jemalloc with profiling and statistics, for memory investigations
  only.
