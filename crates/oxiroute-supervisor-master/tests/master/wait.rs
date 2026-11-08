#[allow(clippy::wildcard_imports)]
use super::*;

fn elapsed_in_wait(
    harness: &Harness,
    extra: &[BorrowedFd<'_>],
    timeout: Option<Duration>,
) -> Duration {
    let started = Instant::now();
    harness.master.wait(extra, timeout).unwrap();
    started.elapsed()
}

#[test]
pub(super) fn an_idle_master_sleeps_for_the_whole_timeout() {
    let harness = Harness::launch("normal");

    let elapsed = elapsed_in_wait(&harness, &[], Some(Duration::from_millis(250)));

    assert!(
        elapsed >= Duration::from_millis(240),
        "woke after {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(2), "woke after {elapsed:?}");
}

#[test]
pub(super) fn a_worker_crash_wakes_the_wait_before_the_timeout() {
    let mut harness = Harness::launch("crash-notice");
    harness.master.notify_active(b"crash").unwrap();

    let elapsed = elapsed_in_wait(&harness, &[], Some(TEST_TIMEOUT));

    assert!(elapsed < Duration::from_secs(2), "woke after {elapsed:?}");
    harness.until(|master| master.state() == MasterState::Failed);

    // A failed master has nothing to wait for; blocking here would hide the failure from its caller.
    let elapsed = elapsed_in_wait(&harness, &[], Some(TEST_TIMEOUT));
    assert!(
        elapsed < Duration::from_millis(100),
        "woke after {elapsed:?}"
    );
}

#[test]
pub(super) fn a_readable_extra_descriptor_wakes_the_wait() {
    let harness = Harness::launch("normal");
    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();

    let elapsed = elapsed_in_wait(&harness, &[reader.as_fd()], Some(TEST_TIMEOUT));

    assert!(elapsed < Duration::from_secs(1), "woke after {elapsed:?}");
}

#[test]
pub(super) fn the_next_state_machine_deadline_bounds_the_wait() {
    let mut harness = Harness::launch("normal");
    harness.replace("hold-activate", 2);
    harness.poll_until(|master| master.state() == MasterState::ActivatingCandidate);

    let started = Instant::now();
    let mut waits = 0;
    while harness.master.state() == MasterState::ActivatingCandidate {
        harness.master.poll(Instant::now()).unwrap();
        harness.master.wait(&[], Some(TEST_TIMEOUT)).unwrap();
        waits += 1;
    }

    // The candidate never answers the activation: its 300 ms deadline ends the wait, not TEST_TIMEOUT.
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(waits < 20, "{waits} waits: the wait spins");
}
