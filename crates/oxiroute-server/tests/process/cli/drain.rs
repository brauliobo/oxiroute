//! `generation drain --wait` and the `undrain` aliases against a real daemon.

use serde_json::Value;

use super::{Daemon, stderr, stdout};

#[tokio::test]
async fn a_generation_drain_waits_only_when_asked_and_no_longer_than_five_minutes() {
    let daemon = Daemon::start().await;

    let too_long = daemon.run(&["--timeout", "6m", "generation", "drain", "--wait"]);
    assert_eq!(too_long.status.code(), Some(3), "{}", stderr(&too_long));
    assert!(
        stderr(&too_long).contains("at most 300s"),
        "{}",
        stderr(&too_long)
    );

    let drained = daemon.run(&[
        "--output",
        "json",
        "--timeout",
        "10s",
        "generation",
        "drain",
        "--wait",
    ]);
    assert!(drained.status.success(), "{}", stderr(&drained));
    let body: Value = serde_json::from_slice(&drained.stdout).expect("JSON");
    assert_eq!(body["outcome"], "drained");
}

#[tokio::test]
async fn undrain_is_the_opposite_of_drain_for_servers_pools_and_listeners() {
    let daemon = Daemon::start().await;
    let line = |words: &[&str]| {
        let output = daemon.run(&[&["--output", "plain"], words].concat());
        assert!(output.status.success(), "{words:?}: {}", stderr(&output));
        stdout(&output)
    };

    line(&["server", "drain", "--pool", "farm", "alpha"]);
    assert_eq!(
        line(&["server", "undrain", "--pool", "farm", "alpha"]),
        "target=farm/alpha setting=state value=ready change=changed\n"
    );
    line(&["pool", "drain", "farm"]);
    assert_eq!(
        line(&["pool", "undrain", "farm"]),
        "target=farm setting=state value=ready change=changed\n"
    );
    line(&["listener", "drain", "traffic"]);
    assert_eq!(
        line(&["listener", "undrain", "traffic"]),
        "target=traffic setting=state value=ready change=changed\n"
    );
}
