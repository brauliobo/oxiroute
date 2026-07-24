# Reverse HTTP/1.1 performance cause report - 2026-07-24

## Result

The only retained change is portable fat LTO with one codegen unit. Its dedicated exact-binary block
measured `+8.13%`; the final independently rebuilt baseline/final block measured `+3.64%`. The
separate final cross-product block measured OxiRoute at 99,131 req/s and nginx at 207,725 req/s, so
nginx remains 2.10x faster in this narrow workload. Every published run had zero failures and zero
non-2xx/3xx responses.

| Comparison | Baseline mean | Candidate mean | Delta |
| --- | ---: | ---: | ---: |
| Generic release vs fat LTO diagnostic binaries | 93,699 | 101,318 | +8.13% |
| Generic release vs final independently rebuilt binary | 94,807 | 98,253 | +3.64% |
| Final OxiRoute vs nginx | 99,131 | 207,725 | nginx 2.10x |

The blocks were measured separately, so their deltas are not compounded. The difference between the
two LTO estimates is host drift, not an additional source change.

## Causes

1. **Build asymmetry was real and actionable.** OxiRoute previously used Cargo's generic release
   defaults. The installed nginx was built with `-march=native -O3 -flto=auto`. Portable fat LTO and
   one codegen unit closed 3.64% to 8.13% in exact-binary blocks without making OxiRoute
   host-specific. A diagnostic native build improved further, but `target-cpu=native` was not
   retained because distributed binaries must remain portable.
2. **Most remaining work is below OxiRoute routing.** `perf` sampling was dominated by Pingora HTTP
   parsing/header ownership, `Bytes` slicing and reference counting, response copying, Tokio
   readiness/waker/semaphore work, timeout registration, and connection-pool release. OxiRoute route
   and endpoint selection were minor self-time contributors.
3. **The proxies optimize different workloads.** nginx's compact C HTTP/1 event loop is very strong
   for one loopback origin and fixed 1 KiB responses. Pingora's production advantages center on
   cross-origin connection reuse, scheduling, memory safety, and replacing OpenResty/Lua at
   Cloudflare scale. Those advantages are mostly absent from this localhost test.
4. **User-space instruction cost remains materially higher.** Diagnostic `perf stat` sampling found
   substantially more user-space instructions per request in OxiRoute. Kernel events were blocked
   by `perf_event_paranoid=2`, so those counts are directional rather than a complete CPU model.

Cloudflare's public context is
[`How we built Pingora`](https://blog.cloudflare.com/how-we-built-pingora-the-proxy-that-connects-cloudflare-to-the-internet/).
It reports production CPU, memory, and connection-reuse improvements, not a reproducible stock
Pingora-versus-nginx localhost requests-per-second benchmark.

## Iterations

| Iteration | Decision | Evidence |
| --- | --- | --- |
| Fat LTO and one codegen unit | Retained | +8.13% dedicated block; +3.64% final block |
| jemalloc global allocator | Rejected | Longer runs collapsed from about 102k to 63k req/s in two jemalloc-linked binaries |
| Singleton endpoint-pool lock bypass | Rejected | +0.09%, within observed noise |
| Reuse one canonicalized path | Rejected | Exploratory exact-binary block regressed 0.63% |
| Avoid cloning request method and URI | Rejected | Longer exploratory block regressed 5.05% |

The jemalloc result is important: the initial short pair looked 1% faster, but longer clean runs
repeatedly exposed severe stalls. Both the allocator and the otherwise neutral singleton path were
removed in follow-up commits. The final source contains no benchmark-specific routing branch.

## Method

- Explicit `GET /payload HTTP/1.1`, 1,024-byte response, 128 persistent connections, one in-flight
  request per connection, 10-second warm-up, and 30-second measurement.
- Proxy CPU 2, origin CPU 3, load-generator CPU 4; ports 49080 and 49081.
- Exact binaries alternated in ABBA order. Each run retained its config, logs, raw output,
  environment, compiler provenance, source commit, worktree state, and binary hashes.
- The generic baseline was built from `88e2b4b`; the final retained source is `e1747d8`.
- Profiling used a separate symbolized frame-pointer build and 19,844 samples with no loss. Its raw
  `perf.data` is diagnostic and is not part of the published benchmark archive.

## Verification

- `CARGO_INCREMENTAL=0 cargo +1.87.0 test --workspace --all-targets --locked`
- `CARGO_INCREMENTAL=0 cargo +1.87.0 clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo +1.87.0 fmt --all -- --check`
- `benchmarks/scripts/validate.sh`
- `git diff --check`

## Limitations

- The workstation was not isolated. External contention produced several documented low outliers,
  which is why decisions use adjacent exact-binary blocks rather than selected global best runs.
- This benchmark does not cover production WAN connection reuse, TLS, HTTP/2, HTTP/3, cache,
  multi-core scaling, uploads, latency percentiles, or memory use.
- Native CPU code generation was diagnostic only. The retained release profile stays portable.
- Profile samples explain where OxiRoute spends time but do not prove that every vendor-level cost
  can be removed without changing Pingora semantics.

Machine-readable measurements and evidence references are in
[`2026-07-24-performance-cause-v1.json`](2026-07-24-performance-cause-v1.json).
