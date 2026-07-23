# Reverse HTTP/1.1 optimization report - 2026-07-23

## Result

Three independently committed changes reduced work in OxiRoute's reverse-proxy request path. The
exact-binary cumulative comparison measured a 4.68% median throughput improvement and a 4.79% mean
improvement, with comparable dispersion and zero failed or non-2xx/3xx requests.

| Binary | R1 req/s | R2 req/s | R3 req/s | Median req/s | Mean req/s | CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Original baseline | 93,418 | 95,899 | 97,125 | 95,899 | 95,481 | 1.61% |
| Cumulative optimized | 97,914 | 101,863 | 100,388 | 100,388 | 100,055 | 1.63% |

The original binary was SHA-256
`5f8b7f378ec9ccd71d4cb987a5f4261315a5957e5ea5c1be181211ff10d2604b`. The cumulative binary was
`b6d66fff911d81677e9a52bbf885463eb3e71bf8b832b2ec03c7731c07f9fbcf` from source commit
`1042e00018c619209286e3f710400b82d4d63ae6`.

## Iterations

| Iteration | Change | Baseline median | Candidate median | Median delta |
| --- | --- | ---: | ---: | ---: |
| 1 | Derive normalized host and client IP only when consumed | 97,999 | 98,175 | +0.18% |
| 2 | Skip round-robin release-side selection locking | 99,268 | 99,152 | -0.12% |
| 3 | Do not install Pingora's disabled compression module | 93,839 | 97,835 | +4.26% |

Iterations 1 and 2 are throughput-neutral within observed noise. They were retained because they
remove allocations and synchronization from the common route without weakening configured dynamic
headers, redirects, least-connections ordering, health behavior, or lease accounting. No material
throughput gain is attributed to either change. Iteration 3 produced the clear measured gain.

Commits:

- `26ca1ed900ea6f12fe96dfe83b23385e3c48f14b` derives dynamic request values lazily.
- `703a3e0b45a19c20ce4627bcb8569d2956722c7b` avoids round-robin release locking.
- `1042e00018c619209286e3f710400b82d4d63ae6` omits disabled downstream compression.

## Method

- Same corrected local-v2 workload: explicit `GET /payload HTTP/1.1`, 1,024-byte response, 128
  persistent connections, one in-flight request per connection, 10-second warm-up, and 30-second
  measurement.
- Baseline and candidate binaries alternated within every iteration and in the cumulative check.
- Three measured repetitions per binary, with proxy CPU 2, origin CPU 3, and load-generator CPU 4.
- Origin port `39080` and proxy port `39081`.
- Every result verified HTTP/1.1, connection reuse, content length, status, and failure count.
- The machine-readable report contains every local run ID and binary hash.

## Verification

- `CARGO_INCREMENTAL=0 cargo +1.87.0 test --workspace --all-targets --locked`
- `CARGO_INCREMENTAL=0 cargo +1.87.0 clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo +1.87.0 fmt --all -- --check`
- `CARGO_INCREMENTAL=0 pnpm test` in `ui`: 63 tests passed.
- `pnpm build` in `ui`.
- `benchmarks/scripts/validate.sh`.
- `git diff --check`.

Rust 1.87's incremental compiler cache panicked after concurrent Cargo invocations. The same gates
passed sequentially with `CARGO_INCREMENTAL=0`; this was a compiler ICE, not a failed product test.

## Limitations

- This is a closed-loop, single-host, plaintext HTTP/1.1 throughput result, not production sizing or
  a tail-latency measurement.
- The workstation was not isolated and used the `powersave` governor with boost and SMT enabled.
- Per-iteration results are intentionally based on adjacent exact binaries. Deltas must not be
  compounded; the separate original-versus-final comparison is the cumulative result.
- No TLS, HTTP/2, HTTP/3, multi-core, remote-network, cache, or forward-proxy result is represented.

Machine-readable measurements are in
[`2026-07-23-optimization-v1.json`](2026-07-23-optimization-v1.json).
