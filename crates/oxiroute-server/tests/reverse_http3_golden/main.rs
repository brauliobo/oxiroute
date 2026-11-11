#![allow(
    dead_code,
    unused_imports,
    clippy::duplicate_mod,
    clippy::cast_possible_truncation,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_wraps
)]
//! Golden corpus of the reverse HTTP/3 serving path.
//!
//! One daemon process serves a reverse HTTP/3 listener whose service uses every route action and policy
//! the path supports. A corpus of requests (the `h3` client crate for ordinary requests, a raw quinn
//! client for protocol violations) is replayed against it and each outcome is normalised: headers are
//! lowercased and sorted, `date` is dropped, large bodies are hashed, an upstream origin logs everything
//! it receives, and the listener access log and counters are read back. The result is compared with
//! the per-area files of `tests/fixtures/reverse_http3_golden/`, recorded from the `h3` crate implementation.
//!
//! - Replay (default): any difference fails with a per-case diff.
//! - Record: `OXIROUTE_INTERNAL_TEST_RECORD_GOLDEN=1` rewrites the fixture from the current implementation;
//!   `OXIROUTE_INTERNAL_TEST_RECORD_CASES=id1,id2,stats` re-records only the listed cases (and the counters when
//!   `stats` is listed), keeping the `deviation` note of each: a deliberate, reviewable change.
//! - A case with a `deviation` note is a recorded difference from the `h3` crate path that RFC 9114/9204 or the
//!   behaviour of that path justifies; it replays exactly like any other case.
//! - A case with `tolerance` is compared without its access log lines, whose content depends on timing.

#[path = "../support/fixtures.rs"]
mod fixture_support;
#[path = "../support/http.rs"]
mod http_support;
#[path = "../support/process.rs"]
mod process_support;
#[path = "../support/mod.rs"]
mod support;

mod cases_cache;
mod cases_fixed;
mod cases_limits;
mod cases_proxy;
mod cases_static;
mod cases_wire;
mod client;
mod config;
mod context;
mod fixture;
mod golden;
mod origin;
mod raw;
mod stats;

use golden::{RECORD_CASES_VARIABLE, RECORD_VARIABLE};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reverse_http3_matches_the_golden_corpus() {
    let mut context = context::Context::start().await;
    cases_fixed::run(&mut context).await;
    context.checkpoint("after_fixed_responses").await;
    cases_static::run(&mut context).await;
    context.checkpoint("after_static_files").await;
    cases_proxy::run(&mut context).await;
    context.checkpoint("after_proxy").await;
    cases_cache::run(&mut context).await;
    context.checkpoint("after_cache").await;
    cases_wire::run(&mut context).await;
    cases_limits::run(&mut context).await;
    let observed = context.finish().await;
    let selected = std::env::var(RECORD_CASES_VARIABLE).ok();
    if selected.is_some() || std::env::var_os(RECORD_VARIABLE).is_some() {
        golden::record(&observed, selected.as_deref());
    } else {
        golden::replay(&observed);
    }
}
