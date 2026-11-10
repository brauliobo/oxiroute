#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;

// The shared harness lives with the crate's robustness test.
#[path = "../../crates/oxiroute-h2/tests/harness/mod.rs"]
mod harness;

const MAX_INPUT_BYTES: usize = 65_536;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    harness::run(&data);
});
