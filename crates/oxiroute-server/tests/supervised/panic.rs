use super::*;

/// A panic inside the listener service is unwound to the service boundary, which fails the generation: the master
/// never reaches `Running` and reaps the worker. It runs the real worker binary, so scripts/test-release-binary.sh uses it
/// to exercise the unwind tables of a post-link-optimised build.
#[test]
pub(super) fn listener_service_panic_fails_the_generation_and_is_reaped() {
    let directory = tempfile::tempdir().expect("supervised panic fixture directory");
    let tcp_address = ports_support::unused_address();
    let config = listeners_only_config(tcp_address, directory.path().join("panic.sock"));
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid panic config"),
        ),
    )
    .expect("write panic config");
    let mut harness = Harness::launch_at_with_failures(
        &path,
        canonical_revision(&path),
        false,
        Some(TEST_LISTENER_PANIC_ENV),
    );
    let started = Instant::now();
    while harness.master.state() != MasterState::Failed {
        let state = harness.master.state();
        assert!(
            !matches!(state, MasterState::ActivatingInitial | MasterState::Running),
            "a panicking listener advanced to {state:?}"
        );
        harness.master.poll(Instant::now()).expect("master poll");
        assert!(started.elapsed() < TEST_TIMEOUT);
        thread::sleep(POLL_INTERVAL);
    }
    harness.verify_reaped();
}
