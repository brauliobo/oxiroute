use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use tempfile::{TempDir, tempdir};

use super::*;
use crate::{
    RecorderEnqueueResult, RecorderFailure, RecorderWorkerPhase, RecordingPathPolicy,
    RecordingStore, RecordingStoreLimits, RtmpCapabilities,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(1);

#[test]
fn recovery_bootstrap_discards_an_oversized_snapshot_without_blocking_start() {
    let mut bootstrap = RecorderBootstrap::default();
    bootstrap.update(
        &MediaEvent::audio(0, Arc::<[u8]>::from(&b"\xaf\x00\x12"[..])).expect("AAC header"),
    );
    bootstrap.update(
        &MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x00\x00\x00\x00\x01"[..]))
            .expect("AVC header"),
    );

    let config = RecorderWorkerConfig {
        max_queue_messages: 2,
        max_queue_bytes: 1024,
        rotation_interval: None,
        shutdown_timeout: Duration::from_secs(1),
        video_codec: None,
        ..RecorderWorkerConfig::default()
    };
    let reserved = MediaEvent::audio(1, Arc::<[u8]>::from(&b"current"[..])).expect("current audio");
    let replay = bootstrap
        .replay_events(config, Some(&reserved))
        .expect("bootstrap fits without the reserved event");

    assert_eq!(replay.events.len(), 2);
    assert!(!replay.reserved_event_fits);
    assert_eq!(bootstrap.events().count(), 2);

    assert!(
        bootstrap
            .replay_events(
                RecorderWorkerConfig {
                    max_queue_messages: 1,
                    ..config
                },
                Some(&reserved)
            )
            .is_none()
    );
    assert_eq!(bootstrap.events().count(), 2);
}

#[test]
fn recovery_gap_rejects_a_stale_keyframe_but_keeps_a_new_one() {
    let mut bootstrap = RecorderBootstrap::default();
    let stale =
        MediaEvent::video(0, Arc::<[u8]>::from(&b"\x17\x01stale"[..])).expect("stale keyframe");
    bootstrap.update(&stale);
    let audio = MediaEvent::audio(1, Arc::<[u8]>::from(&b"audio"[..])).expect("audio");
    bootstrap.mark_recovery_gap(audio.kind(), RecorderEnqueueResult::Inactive);
    assert!(bootstrap.keyframe.is_none());

    let current =
        MediaEvent::video(2, Arc::<[u8]>::from(&b"\x17\x01current"[..])).expect("current keyframe");
    bootstrap.update(&current);
    bootstrap.mark_recovery_gap(current.kind(), RecorderEnqueueResult::Inactive);
    assert_eq!(bootstrap.keyframe, Some(current));
}

#[test]
fn publication_claim_wins_over_process_deadline_cancellation() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    let control = fixture.reaper.shutdown_control();
    fixture.controller.deactivate(1_100);
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    gate.allow_claim();
    assert!(gate.wait_after_claim(TEST_TIMEOUT));

    let budget = Duration::from_millis(250);
    let started = Instant::now();
    fixture.shutdown_owner(None);
    let shutdown = control.initiate_shutdown(started + budget);
    assert!(started.elapsed() < Duration::from_millis(50));
    wait_until(TEST_TIMEOUT, || started.elapsed() >= budget);
    assert!(!matches!(
        fixture
            .controller
            .status()
            .status
            .map(|status| status.phase),
        Some(RecorderWorkerPhase::Failed(
            RecorderFailure::ShutdownTimedOut
        ))
    ));
    gate.allow_publication();
    assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));

    let status = fixture
        .controller
        .status()
        .status
        .expect("completed recorder status");
    assert_eq!(status.phase, RecorderWorkerPhase::Stopped);
    assert_eq!(status.segments_completed, 1);
    assert!(fixture.root.path().join("camera.flv").is_file());
    assert!(partial_files(fixture.root.path()).is_empty());
    assert_eq!(fixture.reaper.queue.status(), (false, 0));
}

#[test]
fn shutdown_control_monotonically_tightens_after_owner_drop() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (owner, reaper) = registry.create_recorder_reaper(1);
    let control = reaper.shutdown_control();
    drop(owner);
    let now = Instant::now();
    let first = now + Duration::from_millis(400);
    let later = now + Duration::from_millis(800);
    let earlier = now + Duration::from_millis(200);

    drop(control.initiate_shutdown(first));
    assert_eq!(reaper.queue.shutdown_deadline(), Some(first));
    drop(control.initiate_shutdown(later));
    assert_eq!(reaper.queue.shutdown_deadline(), Some(first));
    drop(control.initiate_shutdown(earlier));
    assert_eq!(reaper.queue.shutdown_deadline(), Some(earlier));
    assert!(!reaper.queue.status().0);
}

#[test]
fn shutdown_control_tightens_long_owner_drop_cleanup_deadline() {
    let mut fixture = RecorderFixture::new(Duration::from_secs(5));
    let gate = fixture.prepare_publication();
    let control = fixture.reaper.shutdown_control();
    fixture.shutdown_owner(None);
    fixture.controller.deactivate(1_100);
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    let budget = Duration::from_millis(60);
    let started = Instant::now();
    let shutdown = control.initiate_shutdown(started + budget);

    assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
    assert!(started.elapsed() >= budget.saturating_sub(Duration::from_millis(10)));
    assert!(started.elapsed() < Duration::from_millis(250));
    let status = fixture
        .controller
        .status()
        .status
        .expect("cancelled recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );
    assert!(status.recoverable_partial_name.is_some());
}

#[test]
fn recorder_retirement_runs_once_across_shutdown_handle_clones() {
    let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
        live_ingest: true,
        manual_recording: true,
    }));
    let (owner, _reaper) = registry.create_recorder_reaper(1);
    let shutdown = owner.shutdown_handle();
    let clone = shutdown.clone();
    let retired = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&retired);
    shutdown.set_retirement(move || {
        observed.fetch_add(1, Ordering::AcqRel);
    });

    let deadline = Instant::now() + TEST_TIMEOUT;
    drop(owner.initiate_shutdown(deadline));
    assert!(shutdown.wait_until(deadline));
    assert!(clone.wait_until(deadline));
    assert_eq!(retired.load(Ordering::Acquire), 1);
}

#[test]
fn cancellation_claim_wins_at_the_existing_task_deadline() {
    let shutdown_timeout = Duration::from_millis(80);
    let mut fixture = RecorderFixture::new(shutdown_timeout);
    let gate = fixture.prepare_publication();
    let started = Instant::now();
    fixture.controller.deactivate(1_100);
    assert!(gate.wait_before_claim(TEST_TIMEOUT));

    let owner_drop_started = Instant::now();
    fixture.shutdown_owner(None);
    assert!(owner_drop_started.elapsed() < Duration::from_millis(50));

    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert!(started.elapsed() >= shutdown_timeout);
    assert!(started.elapsed() < TEST_TIMEOUT);
    let status = fixture
        .controller
        .status()
        .status
        .expect("cancelled recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );
    let partial = status
        .recoverable_partial_name
        .expect("deadline cancellation preserves the partial");
    assert!(fixture.root.path().join(partial).is_file());
    assert!(fixture.root.path().join("camera.flv").is_file());
    assert_eq!(fixture.reaper.queue.status(), (false, 0));
}

#[test]
fn process_deadline_caps_a_longer_task_deadline() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    fixture.controller.deactivate(1_100);
    assert!(gate.wait_before_claim(TEST_TIMEOUT));

    let budget = Duration::from_millis(60);
    let started = Instant::now();
    fixture.shutdown_owner(Some(started + budget));

    assert!(started.elapsed() < Duration::from_millis(50));
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert!(started.elapsed() >= budget.saturating_sub(Duration::from_millis(10)));
    assert!(started.elapsed() < Duration::from_millis(250));
    let status = fixture
        .controller
        .status()
        .status
        .expect("cancelled recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );
    assert!(status.recoverable_partial_name.is_some());
    assert!(fixture.root.path().join("camera.flv").is_file());
}

#[test]
fn reaper_rejects_and_cleans_up_submissions_after_shutdown() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    fixture.prepare_publication();
    fixture.shutdown_owner(Some(Instant::now()));
    assert_eq!(fixture.reaper.queue.status(), (false, 0));

    let started = Instant::now();
    fixture.controller.deactivate(1_100);

    assert!(started.elapsed() < Duration::from_millis(100));
    wait_until(TEST_TIMEOUT, || {
        fixture.controller.status().status.is_some_and(|status| {
            matches!(
                status.phase,
                RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
            )
        })
    });
    let status = fixture
        .controller
        .status()
        .status
        .expect("rejected recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
    );
    let partial = status
        .recoverable_partial_name
        .expect("rejected submission preserves the partial");
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert!(fixture.root.path().join(partial).is_file());
    assert!(fixture.root.path().join("camera.flv").is_file());
}

#[test]
fn shutdown_closes_intake_and_wakes_a_capacity_blocked_submitter() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    fixture
        .controller
        .start(recorder_context(1_200))
        .expect("start replacement recorder worker");
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                .expect("replacement audio event"),
            1_250,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture
            .controller
            .status()
            .status
            .is_some_and(|status| status.bytes_written > 13)
    });

    let controller = Arc::clone(&fixture.controller);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submitter = thread::spawn(move || {
        assert!(controller.stop(recorder_context(1_300)));
        submitted_tx.send(()).expect("report blocked submission");
    });
    assert!(matches!(
        submitted_rx.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    fixture.shutdown_owner(Some(Instant::now() + Duration::from_millis(60)));
    submitted_rx
        .recv_timeout(TEST_TIMEOUT)
        .expect("closed intake wakes submitter");
    submitter.join().expect("reaper submitter");
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert_eq!(fixture.reaper.queue.status(), (false, 0));
}

#[test]
fn stale_reap_completion_does_not_overwrite_replacement_worker_status() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(80));
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));

    fixture
        .controller
        .start(recorder_context(1_200))
        .expect("start replacement recorder worker");
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                .expect("replacement audio event"),
            1_250,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

    let replacement = fixture.controller.status();
    let replacement_status = replacement.status.expect("replacement worker status");
    assert!(!replacement.stopping);
    assert_eq!(replacement_status.phase, RecorderWorkerPhase::Recording);
    assert!(replacement_status.bytes_written > 13);

    fixture.controller.deactivate(1_300);
    fixture.shutdown_owner(None);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert!(fixture.root.path().join("camera.flv").is_file());
}

#[test]
fn capacity_one_restart_waits_for_the_previous_worker_lease() {
    let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(40), 1);
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));

    let (started_tx, started_rx) = mpsc::channel();
    let mut restarts = Vec::new();
    for _ in 0..2 {
        let controller = Arc::clone(&fixture.controller);
        let started_tx = started_tx.clone();
        restarts.push(thread::spawn(move || {
            started_tx
                .send(controller.start(recorder_context(1_200)))
                .expect("report replacement start");
        }));
    }
    assert!(matches!(
        started_rx.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    for _ in 0..2 {
        started_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("replacement starts after prior lease is reaped")
            .expect("start replacement recorder");
    }
    for restart in restarts {
        restart.join().expect("replacement start thread");
    }

    fixture.controller.deactivate(1_300);
    fixture.shutdown_owner(None);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
}

#[test]
fn shutdown_wakes_a_capacity_one_restart_waiter() {
    let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(500), 1);
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));

    let controller = Arc::clone(&fixture.controller);
    let (started_tx, started_rx) = mpsc::channel();
    let restart = thread::spawn(move || {
        started_tx
            .send(controller.start(recorder_context(1_200)))
            .expect("report replacement start");
    });
    assert!(matches!(
        started_rx.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let shutdown = fixture.initiate_shutdown_owner(Instant::now());
    assert_eq!(
        started_rx
            .recv_timeout(TEST_TIMEOUT)
            .expect("shutdown wakes replacement start"),
        Err(RecorderErrorCode::BackendUnavailable)
    );
    restart.join().expect("replacement start thread");
    assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
}

#[test]
fn continuous_recovery_resumes_the_existing_file_inside_the_interval() {
    assert_continuous_recovery(50_000, "camera-1.flv");
}

#[test]
fn continuous_recovery_starts_the_current_file_after_the_interval() {
    assert_continuous_recovery(62_000, "camera-62.flv");
}

#[test]
fn continuous_recovery_backs_off_while_the_reaper_is_saturated() {
    let mut fixture = RecorderFixture::with_max_active(Duration::from_millis(500), 1);
    fixture.controller.fail_before_process();
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"failure"[..])).expect("failure audio"),
            1_100,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture.controller.status().status.is_some_and(|status| {
            status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
        })
    });
    fixture
        .reaper
        .queue
        .state
        .lock()
        .expect("reaper state")
        .pending = 1;

    let worker_generation = fixture.controller.lock().worker_generation;
    let started = Instant::now();
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"saturated"[..])).expect("saturated audio"),
            1_200,
        ),
        RecorderEnqueueResult::Inactive
    );
    assert!(started.elapsed() < Duration::from_millis(20));
    assert!(fixture.controller.lock().worker.is_some());
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(2, Arc::<[u8]>::from(&b"backoff"[..])).expect("backoff audio"),
            1_201,
        ),
        RecorderEnqueueResult::Inactive
    );
    assert_eq!(
        fixture.controller.lock().worker_generation,
        worker_generation
    );

    fixture
        .reaper
        .queue
        .state
        .lock()
        .expect("reaper state")
        .pending = 0;
    fixture.reaper.queue.available.notify_all();
    thread::sleep(CONTINUOUS_RESTART_DELAY);
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(3, Arc::<[u8]>::from(&b"retry"[..])).expect("retry audio"),
            1_300,
        ),
        RecorderEnqueueResult::Inactive
    );
    wait_until(TEST_TIMEOUT, || {
        let status = fixture.controller.status();
        !status.stopping && status.recovering
    });
    thread::sleep(CONTINUOUS_RESTART_DELAY);
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(4, Arc::<[u8]>::from(&b"continued"[..])).expect("continued audio"),
            1_400,
        ),
        RecorderEnqueueResult::Queued
    );
    fixture.controller.deactivate(1_500);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    fixture.shutdown_owner(None);
}

#[test]
fn reaper_panic_closes_intake_and_transfers_pending_cleanup() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    fixture
        .controller
        .start(recorder_context(1_200))
        .expect("start replacement recorder worker");

    let controller = Arc::clone(&fixture.controller);
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let submitter = thread::spawn(move || {
        assert!(controller.stop(recorder_context(1_300)));
        submitted_tx.send(()).expect("report failed submission");
    });
    assert!(matches!(
        submitted_rx.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    fixture.reaper.queue.panic_reaper();
    submitted_rx
        .recv_timeout(TEST_TIMEOUT)
        .expect("reaper panic wakes blocked submitter");
    submitter.join().expect("reaper submitter");
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    assert_eq!(fixture.reaper.queue.status(), (false, 0));
    fixture.shutdown_owner(None);
}

#[test]
fn segment_finish_panic_reports_the_preserved_partial() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
            1_050,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture
            .controller
            .status()
            .status
            .is_some_and(|status| status.bytes_written > 13)
    });
    fixture.controller.panic_on_finish();

    fixture.controller.deactivate(1_100);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

    let status = fixture
        .controller
        .status()
        .status
        .expect("panicked recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::WorkerPanicked)
    );
    let partial = status
        .recoverable_partial_name
        .expect("finish panic preserves the current partial");
    assert!(fixture.root.path().join(partial).is_file());
    assert!(fixture.root.path().join("camera.flv").is_file());
    fixture.shutdown_owner(None);
}

#[test]
fn post_finish_panic_does_not_report_the_removed_partial() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
            1_050,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture
            .controller
            .status()
            .status
            .is_some_and(|status| status.bytes_written > 13)
    });
    fixture.controller.panic_after_finish();

    fixture.controller.deactivate(1_100);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);

    let status = fixture
        .controller
        .status()
        .status
        .expect("panicked recorder status");
    assert_eq!(
        status.phase,
        RecorderWorkerPhase::Failed(RecorderFailure::WorkerPanicked)
    );
    assert_eq!(status.recoverable_partial_name, None);
    assert!(fixture.root.path().join("camera.flv").is_file());
    assert!(partial_files(fixture.root.path()).is_empty());
    fixture.shutdown_owner(None);
}

#[test]
fn rejected_cleanup_progresses_while_the_reaper_is_still_shutting_down() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    gate.allow_claim();
    assert!(gate.wait_after_claim(TEST_TIMEOUT));

    fixture
        .controller
        .start(recorder_context(1_200))
        .expect("start replacement recorder worker");
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"replacement"[..]))
                .expect("replacement audio event"),
            1_250,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture
            .controller
            .status()
            .status
            .is_some_and(|status| status.bytes_written > 13)
    });

    fixture.shutdown_owner(Some(Instant::now()));
    assert_eq!(fixture.reaper.queue.status(), (false, 1));
    assert!(fixture.controller.stop(recorder_context(1_300)));
    wait_until(TEST_TIMEOUT, || {
        let status = fixture.controller.status();
        !status.stopping
            && status.status.is_some_and(|status| {
                matches!(
                    status.phase,
                    RecorderWorkerPhase::Stopped
                        | RecorderWorkerPhase::Failed(RecorderFailure::ShutdownTimedOut)
                )
            })
    });
    assert_eq!(fixture.reaper.queue.status(), (false, 1));

    gate.allow_publication();
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
}

#[test]
fn shutdown_completion_waits_for_cleanup_but_respects_its_absolute_deadline() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let gate = fixture.prepare_publication();
    fixture.controller.deactivate(1_100);
    assert!(gate.wait_before_claim(TEST_TIMEOUT));
    gate.allow_claim();
    assert!(gate.wait_after_claim(TEST_TIMEOUT));
    let shutdown = fixture.initiate_shutdown_owner(Instant::now() + TEST_TIMEOUT);

    assert!(!shutdown.is_complete());
    let wait_started = Instant::now();
    assert!(!shutdown.wait_until(wait_started + Duration::from_millis(40)));
    assert!(wait_started.elapsed() >= Duration::from_millis(30));

    gate.allow_publication();
    assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));
    assert!(shutdown.is_complete());
}

#[test]
fn recorder_start_after_shutdown_snapshot_is_terminally_rejected() {
    let mut fixture = RecorderFixture::new(Duration::from_millis(500));
    let shutdown = fixture.initiate_shutdown_owner(Instant::now() + TEST_TIMEOUT);
    assert!(fixture.controller.stop(recorder_context(1_100)));
    assert!(shutdown.wait_until(Instant::now() + TEST_TIMEOUT));

    assert_eq!(
        fixture.controller.start(recorder_context(1_200)),
        Err(RecorderErrorCode::BackendUnavailable)
    );
}

struct RecorderFixture {
    root: TempDir,
    owner: Option<Arc<RecorderReaperOwner>>,
    reaper: RecorderReaperHandle,
    controller: Arc<RecorderController>,
}

impl RecorderFixture {
    fn new(shutdown_timeout: Duration) -> Self {
        Self::with_max_active(shutdown_timeout, 2)
    }

    fn with_max_active(shutdown_timeout: Duration, max_active_recorders: usize) -> Self {
        Self::with_recording_policy(shutdown_timeout, max_active_recorders, None, false)
    }

    fn with_recording_policy(
        shutdown_timeout: Duration,
        max_active_recorders: usize,
        rotation_interval: Option<Duration>,
        append_unix_seconds: bool,
    ) -> Self {
        let root = tempdir().expect("recording root");
        let store = RecordingStore::open(
            root.path(),
            RecordingStoreLimits {
                max_bytes: Some(1024 * 1024),
                max_files: Some(8),
                max_active_recorders,
            },
        )
        .expect("recording store");
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: true,
            manual_recording: true,
        }));
        let (owner, reaper) = registry.create_recorder_reaper(1);
        let controller = Arc::new(RecorderController::new(
            RtmpRecorderPolicy::new(
                "archive",
                RtmpRecorderStart::Continuous,
                store,
                RecordingPathPolicy::new(".flv", append_unix_seconds)
                    .expect("recording path policy"),
                RecorderWorkerConfig {
                    max_queue_messages: 4,
                    max_queue_bytes: 1024,
                    rotation_interval,
                    shutdown_timeout,
                    video_codec: None,
                    ..RecorderWorkerConfig::default()
                },
            ),
            Arc::<[u8]>::from(&b"camera"[..]),
            reaper.clone(),
            1_000,
        ));
        controller
            .start(recorder_context(1_000))
            .expect("start recorder worker");
        Self {
            root,
            owner: Some(owner),
            reaper,
            controller,
        }
    }

    fn prepare_publication(&self) -> Arc<crate::recording_store::RecordingPublicationGate> {
        assert_eq!(
            self.controller.try_enqueue(
                MediaEvent::audio(0, Arc::<[u8]>::from(&b"audio"[..])).expect("audio event"),
                1_050,
            ),
            RecorderEnqueueResult::Queued
        );
        wait_until(TEST_TIMEOUT, || {
            self.controller
                .status()
                .status
                .is_some_and(|status| status.segments_started == 1 && status.bytes_written > 13)
        });
        self.controller.install_publication_gate()
    }

    fn shutdown_owner(&mut self, deadline: Option<Instant>) {
        let owner = self.owner.take().expect("recorder reaper owner");
        if let Some(deadline) = deadline {
            let _ = owner.initiate_shutdown(deadline);
        }
        drop(owner);
    }

    fn initiate_shutdown_owner(&mut self, deadline: Instant) -> RtmpRecorderShutdown {
        let owner = self.owner.take().expect("recorder reaper owner");
        let shutdown = owner.initiate_shutdown(deadline);
        drop(owner);
        shutdown
    }
}

fn recorder_context(at_unix_ms: u64) -> RecorderCommandContext {
    RecorderCommandContext {
        stream_id: StreamId::new(),
        publisher_session_id: SessionId::new(),
        recorder_id: RecorderId::new(),
        operation_id: OperationId::new(),
        at_unix_ms,
    }
}

fn assert_continuous_recovery(recovery_at_unix_ms: u64, expected_file: &str) {
    let mut fixture = RecorderFixture::with_recording_policy(
        Duration::from_millis(500),
        1,
        Some(Duration::from_mins(1)),
        true,
    );
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(0, Arc::<[u8]>::from(&b"first"[..])).expect("initial audio"),
            1_050,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture
            .controller
            .status()
            .status
            .is_some_and(|status| status.bytes_written > 13)
    });
    assert!(fixture.controller.stop(recorder_context(1_100)));
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    let initial_file = fixture.root.path().join("camera-1.flv");
    let initial_size = fs::metadata(&initial_file)
        .expect("initial interval file")
        .len();

    fixture
        .controller
        .start(recorder_context(1_200))
        .expect("replacement recorder");
    fixture.controller.fail_before_process();
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(1, Arc::<[u8]>::from(&b"failure"[..])).expect("failure audio"),
            1_250,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture.controller.status().status.is_some_and(|status| {
            status.phase == RecorderWorkerPhase::Failed(RecorderFailure::Open)
        })
    });

    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(2, Arc::<[u8]>::from(&b"recover"[..])).expect("recovery trigger"),
            recovery_at_unix_ms,
        ),
        RecorderEnqueueResult::Inactive
    );
    wait_until(TEST_TIMEOUT, || {
        let status = fixture.controller.status();
        !status.stopping && status.recovering
    });
    let failed_generation = fixture.controller.lock().worker_generation;
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(3, Arc::<[u8]>::from(&b"backoff"[..])).expect("backoff audio"),
            recovery_at_unix_ms + 1,
        ),
        RecorderEnqueueResult::Inactive
    );
    assert_eq!(
        fixture.controller.lock().worker_generation,
        failed_generation,
        "recovery restarted before its bounded delay"
    );
    thread::sleep(CONTINUOUS_RESTART_DELAY);
    assert_eq!(
        fixture.controller.try_enqueue(
            MediaEvent::audio(4, Arc::<[u8]>::from(&b"continued"[..])).expect("continued audio"),
            recovery_at_unix_ms + 2,
        ),
        RecorderEnqueueResult::Queued
    );
    wait_until(TEST_TIMEOUT, || {
        fixture.controller.status().status.is_some_and(|status| {
            status.phase == RecorderWorkerPhase::Recording && status.bytes_written > 13
        })
    });

    assert_recovery_output(
        &mut fixture,
        recovery_at_unix_ms,
        expected_file,
        &initial_file,
        initial_size,
    );
}

fn assert_recovery_output(
    fixture: &mut RecorderFixture,
    recovery_at_unix_ms: u64,
    expected_file: &str,
    initial_file: &Path,
    initial_size: u64,
) {
    fixture.controller.deactivate(recovery_at_unix_ms + 3);
    wait_until(TEST_TIMEOUT, || fixture.reaper.queue.status().1 == 0);
    let recovered_status = fixture
        .controller
        .status()
        .status
        .expect("recovered recorder status");
    assert!(recovered_status.events_dropped >= 3);
    assert_eq!(recovered_status.segments_started, 2);
    assert!(recovered_status.bytes_written > initial_size);
    assert!(fixture.root.path().join(expected_file).is_file());
    if expected_file == "camera-1.flv" {
        assert!(
            fs::metadata(initial_file)
                .expect("resumed interval file")
                .len()
                > initial_size
        );
    } else {
        assert_eq!(
            fs::metadata(initial_file)
                .expect("prior interval file")
                .len(),
            initial_size
        );
    }
    fixture.shutdown_owner(None);
}

fn partial_files(root: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(root)
        .expect("recording root")
        .map(|entry| entry.expect("recording entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".partial"))
        })
        .collect()
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(REAPER_POLL_INTERVAL);
    }
}
