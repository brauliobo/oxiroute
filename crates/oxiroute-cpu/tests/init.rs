//! `init` sets the level of the process: a configured level wins over `OXIROUTE_CPU_LEVEL`, is clamped to the hardware
//! and stays for every later dispatch. The environment is read by each call, so the cases run in a child with it set.

use oxiroute_cpu::{ENV, Level, init, level};
use std::process::Command;

const CHILD: &str = "child_sets_the_level_over_a_bad_environment";

#[test]
fn child_sets_the_level_over_a_bad_environment() {
    if std::env::var(ENV).as_deref() != Ok("avx") {
        return;
    }
    let refused = init(None).unwrap_err().to_string();
    assert_eq!(
        refused,
        "OXIROUTE_CPU_LEVEL=avx: expected scalar, sse2, sse42, avx2 or avx512"
    );
    for configured in Level::ALL {
        assert_eq!(init(Some(configured)), Ok(configured.clamp()));
        assert_eq!(level(), configured.clamp());
    }
    assert_eq!(init(Some(Level::Scalar)), Ok(Level::Scalar));
    assert_eq!(level(), Level::Scalar);
}

#[test]
fn a_configured_level_replaces_the_environment_and_an_invalid_environment_is_an_error() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--test-threads=1"])
        .env(ENV, "avx")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn without_a_configured_level_the_environment_lowers_the_hardware_level() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_lowers_to_the_environment",
            "--test-threads=1",
        ])
        .env(ENV, "sse2")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn child_lowers_to_the_environment() {
    if std::env::var(ENV).as_deref() == Ok("sse2") {
        assert_eq!(init(None), Ok(Level::Sse2.clamp()));
        assert_eq!(level(), Level::Sse2.clamp());
    }
}
