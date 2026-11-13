//! `OXIROUTE_CPU_LEVEL` is read once per process, so each case runs this test binary again with it set.

use oxiroute_cpu::{ENV, Level, level};
use std::process::Command;

const CHILD: &str = "child_reports_the_level";

/// Runs only as the child of the tests below (or under an `OXIROUTE_CPU_LEVEL` the caller set): the level is the
/// requested one, lowered to the hardware.
#[test]
fn child_reports_the_level() {
    let actual = level();
    let requested = std::env::var(ENV)
        .ok()
        .map(|name| Level::parse(&name).unwrap());
    assert_eq!(
        actual,
        requested.map_or(Level::hardware(), |requested| requested
            .min(Level::hardware()))
    );
}

fn child(value: &str) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    Command::new(exe)
        .args(["--exact", CHILD, "--test-threads=1"])
        .env(ENV, value)
        .output()
        .unwrap()
}

#[test]
fn every_requested_level_is_honoured_or_clamped() {
    for requested in Level::ALL {
        let out = child(requested.name());
        assert!(
            out.status.success(),
            "{requested:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn an_unknown_level_is_refused() {
    let out = child("avx");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("OXIROUTE_CPU_LEVEL=avx: expected scalar, sse42, avx2 or avx512")
    );
}
