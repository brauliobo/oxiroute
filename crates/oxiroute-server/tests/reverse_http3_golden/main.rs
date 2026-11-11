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
//! `tests/fixtures/reverse_http3_golden.json`, which was recorded from the `h3` crate implementation.
//!
//! - Replay (default): any difference fails with a per-case diff.
//! - Record: `OXIROUTE_INTERNAL_TEST_RECORD_GOLDEN=1` rewrites the fixture from the current implementation.
//! - A case with `tolerance` may legitimately differ where the RFCs leave the choice open: its steps
//!   accept the listed `accept` alternatives (each overrides keys of the recorded step) and its access
//!   log lines are not compared.

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
mod golden;
mod origin;
mod raw;
mod stats;

use golden::RECORD_VARIABLE;

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
    if std::env::var_os(RECORD_VARIABLE).is_some() {
        golden::record(&observed);
    } else {
        golden::replay(&observed);
    }
}
