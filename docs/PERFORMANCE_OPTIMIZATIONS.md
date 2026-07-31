# HTTP/1 performance optimization record

This document records the reverse HTTP/1.1 optimization work completed on 2026-07-31. It separates
retained source changes from rejected experiments and limits every performance claim to the evidence
that supports it.

## Scope

The benchmark lane is one Linux host, loopback TCP, one proxy worker, 128 persistent HTTP/1.1
connections, one in-flight request per connection, and a fixed 1 KiB response from a dedicated nginx
origin. Proxy, origin, and load generator are pinned to separate CPUs. This lane does not represent
TLS, HTTP/2, HTTP/3, cache, uploads, remote networks, latency percentiles, or multicore scaling.

## Retained changes

| Change | Commit | Evidence |
| --- | --- | --- |
| Reuse owned upstream preread body bytes | `0e0327c` | Targeted preread copy stacks disappeared; instructions/request improved about 0.99% |
| Borrow unchanged upstream request headers | `1f46d38` | Dedicated ABBA mean improved 15.21%; request-header clone stacks disappeared |
| Keep response task input and output inline | `4f2ef94`, `e93b6e2` | Removed the two hot per-batch `Vec` allocations without changing task order or spill behavior |
| Defer redundant pooled-stream readiness validation | `21a2264` | Instructions/request improved about 0.44%; checkout validation and idle monitoring remain |
| Cache physical peer identity | `7251517` | `getpeername` fell from 9,999 calls per 10,000 requests to calls on physical connection creation |
| Replace H1 Tokio MPSC handoffs with bounded SPSC handoffs | `29a786d` | Dedicated paired gain 3.695%, 95% CI `[0.924%, 6.542%]`; instructions/request fell 3.272% |

The SPSC implementation preserves Pingora's independent request and response pumps. Fixed capacity
keeps backpressure explicit, and tests cover FIFO order, saturation, reservation cancellation, lost
wakeups, endpoint drop, panic during value drop, early responses, reset, downstream disconnect,
upgrades, and H2 multi-DATA uploads.

Correctness work discovered during optimization is retained separately from performance claims:

- `1cf931e` preserves the final response after HEAD informational responses.
- `0f4b5be` corrects the vendored H2 empty-DATA/EOS test lifecycle for `h2` 0.4.15.
- `fd86451` makes generation publication, shutdown, recorder finalization, and process deadlines
  transactional under concurrent status, reload, publisher, and shutdown activity.

## Rejected experiments

| Experiment | Reason rejected |
| --- | --- |
| OxiRoute-local allocation cleanup | Throughput result was noisy and did not meet the retention threshold |
| Move response-header ownership | Removed a clone but was throughput-neutral and increased API risk |
| Full socket-level zero-copy response body parser | Review found stream desynchronization, unsafe buffer handling, upgrade loss, and memory amplification before benchmarking |
| Persistent pool entries | Only about 3-6% appeared directly removable while stale-idle, fairness, endpoint-change, and retry semantics became substantially riskier |
| Host-native CPU build | Faster diagnostically, but distributed release binaries must remain portable |

No rejected source experiment remains in the production path.

## Accepted measurements

The pre-SPSC cumulative binary was measured against the original binary in four exact-binary ABBA
blocks. Three stable blocks showed a 4.927% average gain. The directly matched PMU pair improved:

| Metric | Cumulative versus original |
| --- | ---: |
| Cycles/request | -5.40% |
| Instructions/request | -5.66% |
| Task-clock/request | -5.47% |
| Branches/request | -5.95% |
| Branch misses/request | -22.29% |
| Cache references/request | -3.11% |
| Cache misses/request | -5.53% |

The stable rotated product comparison measured the cumulative binary at 103,514 req/s, nginx at
205,173 req/s, and HAProxy at 165,014 req/s. OxiRoute reached 50.45% of nginx and 62.73% of HAProxy
in this narrow lane.

The SPSC change then received a separate four-block ABBA run:

| Metric | SPSC versus cumulative |
| --- | ---: |
| Paired geometric throughput | +3.695% |
| 95% t confidence interval | `[+0.924%, +6.542%]` |
| Cycles/request | -4.077% |
| Instructions/request | -3.272% |
| Branch misses/request | -7.068% |

The initial SPSC memory comparison was invalidated by unmatched process state. A repeated matched
capture, and the final exact-binary capture below, both show that the change does not increase
resident memory.

## Final binary verification

The committed final production binary from `fd86451150cb9780a3ec26d819c598be17367c90` has SHA-256
`dae0f391e98acd36c52e66381ace09b1df644f61fd2e80aa5df48735caf81210`. Its exact original comparator
has SHA-256 `0c0f6b26cad8557db00b73959595b7049cb531fb282cfd1a44028dedae33cf87`.

Adjacent final/original PMU runs had 100% enabled time and no multiplexing:

| Metric | Final versus original |
| --- | ---: |
| Cycles/request | -14.443% |
| Instructions/request | -10.651% |
| Branches/request | -10.618% |
| Branch misses/request | -24.114% |
| Cache references/request | -5.036% |
| Cache misses/request | -7.694% |
| Median RSS | -2.059% |
| Median PSS | -1.951% |

Both variants used 10 threads. The 120 samples per variant were stable: RSS coefficients of
variation were 0.342% original and 0.385% final; PSS coefficients were 0.249% and 0.391%.

A frame-pointer diagnostic build from the same source produced 9,978 samples, zero lost samples,
100% frame coverage, and 94.90% deep callchains. It confirmed the intended mechanisms:

- H1 MPSC creation, semaphore, and waker families remained absent.
- The bounded SPSC family used 1,984 sampled cycles/request with no sampled allocator, mutex, or
  waker leaf.
- The request-header clone and `getpeername` families remained absent.
- The old `BodyReader::prepare_buf`, `BodyReader::finish_body_buf`, and proxy-body copy paths remained
  absent.
- H1 waker cost was 194.8 sampled cycles/request, down from 328.1 in the original profile.

The frame-pointer binary is diagnostic, not the preserved production executable, so profile-period
comparisons are used only for mechanism attribution. PMU counters from the exact production binary
are the performance evidence.

## Invalid final throughput runs

Two additional exact-final ABBA sequences completed with zero request or status failures, but neither
is accepted as throughput evidence. Long-lived unrelated workloads produced host load averages above
five and occupied benchmark CPUs. External controls fell 16-30% from the stable comparison, original
observations ranged from 43.6k to 98.7k req/s, orientation effects were large, and both paired 95%
confidence intervals crossed zero. No result was excluded or selectively replaced.

These invalid runs do not override the stable dedicated optimization blocks or the exact-final PMU
result. They do prevent publishing a new absolute req/s value for the final binary on 2026-07-31.

## Verification

- `cargo fmt --all -- --check`
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings`
- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets`
- Vendored Pingora H2 server tests: 6 passed
- Vendored Pingora accept-gate race tests: 4 passed
- Final benchmark and profile workloads: zero failed requests and zero non-success responses

Local raw evidence is retained in the ignored directories
`benchmarks/generated/optimization-cumulative-20260731T061210Z`,
`benchmarks/generated/optimization-spsc-final-20260731T083538Z`, and
`benchmarks/generated/optimization-final-20260731T124855Z`.
