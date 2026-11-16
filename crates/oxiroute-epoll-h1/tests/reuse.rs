#![allow(clippy::pedantic)]

mod support;

use oxiroute_epoll_h1::{Config, Timeouts};
use support::{Fixture, Knobs, sized, test_config};

const SIZES: [usize; 11] = [
    1, 1023, 16_000, 16_384, 20_000, 32_768, 65_536, 100_001, 1_048_576, 4_194_304, 16_777_216,
];

fn buffered() -> Config {
    Config {
        splice: None,
        ..test_config()
    }
}

fn assert_one_upstream_over(config: Config, sizes: &[usize]) {
    let fixture = Fixture::with(config, Timeouts::default(), Knobs::default());
    let mut wire = fixture.wire();
    for len in sizes {
        wire.get(&format!("/size/{len}"));
        assert!(wire.reply().body == sized(*len), "{len}");
        assert_eq!(fixture.origin.accepted(), 1, "after {len}");
    }
}

#[test]
fn a_mixed_sequence_of_sizes_reuses_one_upstream_connection() {
    assert_one_upstream_over(test_config(), &SIZES);
}

#[test]
fn a_mixed_sequence_of_sizes_reuses_one_upstream_connection_without_splice() {
    assert_one_upstream_over(buffered(), &SIZES);
}

#[test]
fn repeated_large_responses_reuse_one_upstream_connection() {
    assert_one_upstream_over(test_config(), &[1_000_000; 60]);
}

#[test]
fn repeated_large_responses_reuse_one_upstream_connection_without_splice() {
    assert_one_upstream_over(buffered(), &[1_000_000; 60]);
}

#[test]
fn repeated_small_responses_reuse_one_upstream_connection() {
    assert_one_upstream_over(test_config(), &[1000; 200]);
}
