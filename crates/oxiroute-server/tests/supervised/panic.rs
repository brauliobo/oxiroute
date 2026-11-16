use super::*;

const TEST_ENGINE_PANIC_ENV: &str = "OXIROUTE_INTERNAL_TEST_ENGINE_PANIC";

fn listener_panic_config(directory: &Path) -> (ConfigDraft, SocketAddr) {
    let address = ports_support::unused_address();
    (
        listeners_only_config(address, directory.join("panic.sock")),
        address,
    )
}

fn epoll_proxy_config() -> (ConfigDraft, SocketAddr) {
    let address = ports_support::unused_address();
    (
        h1_wire::config(address, ports_support::unused_address()),
        address,
    )
}

fn panicking_harness(
    directory: &Path,
    (config, address): (ConfigDraft, SocketAddr),
    variable: &str,
) -> (Harness, SocketAddr) {
    let path = directory.join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid panic config"),
        ),
    )
    .expect("write panic config");
    let harness =
        Harness::launch_at_with_failures(&path, canonical_revision(&path), false, Some(variable));
    (harness, address)
}

fn poll_until(harness: &mut Harness, reached: impl Fn(MasterState) -> bool) {
    let started = Instant::now();
    while !reached(harness.master.state()) {
        harness.master.poll(Instant::now()).expect("master poll");
        let state = harness.master.state();
        assert!(started.elapsed() < TEST_TIMEOUT, "stuck in {state:?}");
        thread::sleep(POLL_INTERVAL);
    }
}

/// A panic inside the listener service is unwound to the service boundary, which fails the generation: the master
/// never reaches `Running` and reaps the worker. It runs the real worker binary, so scripts/test/release/binary.sh uses it
/// to exercise the unwind tables of a post-link-optimised build.
#[test]
pub(super) fn listener_service_panic_fails_the_generation_and_is_reaped() {
    let directory = tempfile::tempdir().expect("supervised panic fixture directory");
    let (mut harness, _) = panicking_harness(
        directory.path(),
        listener_panic_config(directory.path()),
        TEST_LISTENER_PANIC_ENV,
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

/// A panic on an epoll engine thread, after the generation runs, fails the generation instead of leaving a listener
/// that accepts connections nothing serves.
#[test]
pub(super) fn epoll_engine_thread_panic_fails_the_generation_and_is_reaped() {
    let directory = tempfile::tempdir().expect("supervised panic fixture directory");
    let (mut harness, address) = panicking_harness(
        directory.path(),
        epoll_proxy_config(),
        TEST_ENGINE_PANIC_ENV,
    );
    poll_until(&mut harness, |state| state == MasterState::Running);
    let mut client = TcpStream::connect(address).expect("connect to the epoll listener");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .expect("send the request that panics the engine thread");
    poll_until(&mut harness, |state| state == MasterState::Failed);
    harness.verify_reaped();
}
