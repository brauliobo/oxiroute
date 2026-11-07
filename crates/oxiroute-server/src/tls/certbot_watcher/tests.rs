use std::{sync::mpsc::TryRecvError, time::Instant};

use notify::{
    Event, EventKind,
    event::{AccessKind, AccessMode, Flag},
};

use super::*;

#[test]
fn bounded_wake_queue_coalesces_events() {
    let (wake, receiver, monitor) = WakeQueue::new();

    wake.event();
    wake.event();
    wake.event();

    receiver.try_recv().unwrap();
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(monitor.status().coalesced_events, 2);
    assert!(!monitor.status().degraded);
}

#[test]
fn notify_access_open_and_read_events_are_ignored() {
    let (wake, receiver, monitor) = WakeQueue::new();
    for kind in [
        AccessKind::Read,
        AccessKind::Open(AccessMode::Read),
        AccessKind::Close(AccessMode::Read),
    ] {
        handle_notify_event(&wake, Ok(Event::new(EventKind::Access(kind))));
    }

    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(monitor.status().ignored_access_events, 3);
}

#[test]
fn backend_degradation_and_recovery_are_visible() {
    let (wake, receiver, monitor) = WakeQueue::new();

    wake.backend_error();
    receiver.try_recv().unwrap();
    assert!(monitor.status().degraded);

    wake.backend_recovered();
    let status = monitor.status();
    assert!(!status.degraded);
    assert_eq!(status.backend_errors, 1);
    assert_eq!(status.watch_recoveries, 1);
}

#[test]
fn notify_overflow_rescan_flag_marks_degradation() {
    let (wake, receiver, monitor) = WakeQueue::new();
    let overflow = Event::new(EventKind::Other).set_flag(Flag::Rescan);

    handle_notify_event(&wake, Ok(overflow));

    receiver.try_recv().unwrap();
    assert!(monitor.status().degraded);
}

#[test]
fn validates_identity_and_timing_bounds() {
    let valid = CertbotWatcherConfig::default();
    assert!(matches!(
        validate_configuration(0, valid),
        Err(CertbotWatcherError::NoIdentities)
    ));
    assert!(matches!(
        validate_configuration(MAX_RECONCILERS + 1, valid),
        Err(CertbotWatcherError::TooManyIdentities { .. })
    ));
    assert!(matches!(
        validate_configuration(
            1,
            CertbotWatcherConfig {
                rescan_interval: MIN_RESCAN_INTERVAL
                    .checked_sub(Duration::from_millis(1))
                    .expect("minimum rescan interval exceeds one millisecond"),
                ..valid
            }
        ),
        Err(CertbotWatcherError::RescanIntervalTooShort)
    ));
    assert!(matches!(
        validate_configuration(
            1,
            CertbotWatcherConfig {
                event_debounce: Duration::ZERO,
                ..valid
            }
        ),
        Err(CertbotWatcherError::InvalidEventDebounce)
    ));
    assert!(matches!(
        validate_configuration(
            1,
            CertbotWatcherConfig {
                event_max_delay: valid.rescan_interval + Duration::from_millis(1),
                ..valid
            }
        ),
        Err(CertbotWatcherError::InvalidEventMaxDelay)
    ));
}

#[test]
fn debounce_deadline_is_quiet_period_bounded_by_max_delay() {
    let start = Instant::now();
    let mut window = DebounceWindow::new(start);
    let debounce = Duration::from_millis(100);
    let max_delay = Duration::from_millis(250);
    assert_eq!(window.deadline(debounce, max_delay), start + debounce);

    window.note_event(start + Duration::from_millis(80));
    assert_eq!(
        window.deadline(debounce, max_delay),
        start + Duration::from_millis(180)
    );
    window.note_event(start + Duration::from_millis(240));
    assert_eq!(window.deadline(debounce, max_delay), start + max_delay);
}
