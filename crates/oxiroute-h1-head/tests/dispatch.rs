//! The scanners at the CPU level the process dispatched to, against the byte-at-a-time specification, on inputs small
//! enough to run under an emulated CPU (`scripts/test/cpu-targets.sh` runs this binary on every CPU model). The
//! exhaustive comparisons are `reference_scan.rs`, which runs natively.

mod common;

use common::same;
use oxiroute_cpu::level;

#[test]
fn the_scanners_run_at_the_level_the_environment_expects() {
    if let Ok(expected) = std::env::var("OXIROUTE_EXPECT_SELECTED_LEVEL") {
        assert_eq!(level().name(), expected);
    }
}

#[test]
fn the_corpora_and_the_prefixes_of_them() {
    same::request_corpus_prefixes();
    same::response_corpus_prefixes();
}

#[test]
fn mutations_of_the_corpora() {
    same::mutated_corpora(1_500);
}

#[test]
fn generated_heads_across_the_vector_steps() {
    same::generated_requests(300);
    same::generated_responses(300);
}

#[test]
fn the_limits_of_names_targets_methods_and_header_counts() {
    same::limits_of_names_targets_methods_and_header_counts();
}
