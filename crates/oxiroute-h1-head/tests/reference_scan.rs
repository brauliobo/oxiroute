//! The scanners against the byte-at-a-time specification of `common::reference_scan`, at every level the CPU runs: the
//! same verdict, the same miss reason, and on a head the same offsets, header records and facts, for the hand-written
//! corpora, prefixes of each, mutations of them and generated heads whose names, values and targets cross the 32-byte
//! steps of the vector scanners. `dispatch.rs` replays a small share of these under emulated CPUs.

mod common;

use common::same;

#[test]
fn the_request_corpus_and_the_prefixes_of_it() {
    same::request_corpus_prefixes();
}

#[test]
fn the_response_corpus_and_the_prefixes_of_it() {
    same::response_corpus_prefixes();
}

#[test]
fn mutations_of_the_corpora() {
    same::mutated_corpora(60_000);
}

#[test]
fn generated_request_heads_with_names_values_and_targets_across_the_vector_steps() {
    same::generated_requests(20_000);
}

#[test]
fn generated_response_heads_with_names_values_and_reasons_across_the_vector_steps() {
    same::generated_responses(20_000);
}

#[test]
fn the_limits_of_names_targets_methods_and_header_counts() {
    same::limits_of_names_targets_methods_and_header_counts();
}
