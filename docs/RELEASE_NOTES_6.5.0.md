# OxiRoute 6.5.0 (in preparation)

## Request floor (HTTP/1 lane)

- A proxied HTTP/1.1 keep-alive request on the fast lane needs about 6% to 7% fewer user instructions (shipping build:
  27.2k to 25.5k for a 1 KiB response, 28.9k to 27.2k with header rules, 28.2k to 26.6k for a 1 KiB POST, 8.0k to 7.5k
  for a direct response) and makes 4 fewer allocator calls. The direct response is now ahead of nginx in rate and CPU
  per request; the proxied rows still trail it by 10% to 16% in rate on the same host. Details:
  [results](developer/PERF_NEXT_REQUEST_FLOOR_RESULTS.md).
- Deadlines of the HTTP/1 and HTTP/2 lanes reuse timers kept per worker thread; the lane applies the origin read limits
  itself. No behaviour change.
- New build feature `alloc-count` (test and benchmark builds only) and the `lane_allocations` test that holds the
  allocator calls per request of each route shape to a ceiling.
- Benchmark tooling: `vs_syscalls.py` (system calls per request of OxiRoute, nginx and HAProxy; OxiRoute makes the
  fewest on the proxied rows), `source_profile.py`, `run.py --alloc-internal`.

No option was added and no default was changed.
