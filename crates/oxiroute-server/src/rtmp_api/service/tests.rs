use super::*;
use crate::operational_event::OperationalEvent;

#[test]
fn initial_and_reconnect_cursors_are_distinct_and_stable() {
    let initial = parse_event_stream_cursor(None, None).expect("initial cursor");
    assert_eq!(initial.after, None);
    assert_eq!(initial.limit, EVENT_STREAM_BATCH_LIMIT);

    let reconnect =
        parse_event_stream_cursor(Some("after=4&limit=3"), Some("9")).expect("reconnect cursor");
    assert_eq!(reconnect.after, Some(9));
    assert_eq!(reconnect.limit, 3);
    assert!(parse_event_stream_cursor(Some("limit=65"), None).is_err());
}

#[test]
fn sse_frames_include_cursors_and_bounded_heartbeat() {
    let event = OperationalEvent {
        cursor: 7,
        timestamp_unix_ms: None,
        event: crate::operational_event::EventName::GenerationPrepare,
        outcome: crate::operational_event::EventOutcome::Prepared,
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let frame = operational_frame(&event);

    assert!(String::from_utf8_lossy(&frame).contains("id: 7\nevent: generation_prepare"));
    assert!(frame.len() <= EVENT_STREAM_FRAME_LIMIT);
    assert_eq!(heartbeat_frame(), b": heartbeat\n\n");
    assert!(ready_frame(u64::MAX).len() <= EVENT_STREAM_FRAME_LIMIT);
    assert!(resync_frame(1, Some(2), 3).len() <= EVENT_STREAM_FRAME_LIMIT);
}

#[test]
fn redacted_typed_event_data_cannot_copy_raw_credentials_into_sse() {
    let event = OperationalEvent {
        cursor: 11,
        timestamp_unix_ms: None,
        event: crate::operational_event::EventName::Unknown,
        outcome: crate::operational_event::EventOutcome::Unknown,
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let frame = operational_frame(&event);
    let frame = String::from_utf8(frame).expect("SSE frame");

    assert!(frame.contains(r#""event":"unknown""#));
    assert!(!frame.contains("private-key-secret"));
    assert!(!frame.contains("session-secret"));
}

#[test]
fn version_one_sse_keeps_the_shipped_certificate_activation_spellings() {
    let event = OperationalEvent {
        cursor: 12,
        timestamp_unix_ms: None,
        event: crate::operational_event::EventName::CertificateActivation,
        outcome: crate::operational_event::EventOutcome::Activated,
        revision: None,
        certificate: Some("edge".into()),
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let frame = String::from_utf8(operational_frame(&event)).expect("SSE frame");

    assert!(frame.contains("event: certificate_activated"));
    assert!(frame.contains(r#""event":"certificate_activation""#));

    let event = OperationalEvent {
        event: crate::operational_event::EventName::CertificateRevocation,
        ..event
    };
    let frame = String::from_utf8(operational_frame(&event)).expect("SSE frame");
    assert!(frame.contains("event: unknown"));
    assert!(frame.contains(r#""event":"unknown""#));
}

#[test]
fn version_two_sse_uses_the_complete_matching_event_vocabulary() {
    let event = OperationalEvent {
        cursor: 13,
        timestamp_unix_ms: None,
        event: crate::operational_event::EventName::CertificateRevocation,
        outcome: crate::operational_event::EventOutcome::Requested,
        revision: None,
        certificate: Some("edge".into()),
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let frame = String::from_utf8(operational_frame_v2(&event)).expect("SSE frame");

    assert!(frame.contains("event: certificate_revocation"));
    assert!(frame.contains(r#""event":"certificate_revocation""#));
    assert!(frame.contains(r#""outcome":"requested""#));
}
