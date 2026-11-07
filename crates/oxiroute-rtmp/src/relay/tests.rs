use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use super::*;

struct SequenceResolver {
    answers: Mutex<VecDeque<Result<Vec<SocketAddr>, io::ErrorKind>>>,
    calls: AtomicUsize,
}

impl SequenceResolver {
    fn new(answers: impl IntoIterator<Item = Result<Vec<SocketAddr>, io::ErrorKind>>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl RtmpDnsResolver for SequenceResolver {
    fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        match self
            .answers
            .lock()
            .expect("test resolver mutex poisoned")
            .pop_front()
            .unwrap_or(Err(io::ErrorKind::NotFound))
        {
            Ok(addresses) => Ok(addresses),
            Err(kind) => Err(io::Error::from(kind)),
        }
    }
}

fn test_resolver(
    initial: SocketAddr,
    policy: RtmpOutboundPolicy,
    listeners: impl IntoIterator<Item = SocketAddr>,
    answers: impl IntoIterator<Item = Result<Vec<SocketAddr>, io::ErrorKind>>,
) -> (RtmpDestinationResolver, Arc<SequenceResolver>) {
    let resolver = Arc::new(SequenceResolver::new(answers));
    let destination = RtmpDestinationResolver::from_startup_with_resolver(
        "relay.example",
        initial.port(),
        RtmpTransport::Rtmp,
        [initial],
        policy,
        listeners,
        Duration::from_millis(5),
        Arc::clone(&resolver) as Arc<dyn RtmpDnsResolver>,
    )
    .expect("startup destination");
    (destination, resolver)
}

#[test]
fn dns_refresh_rotates_an_address_once_per_bounded_interval() {
    let initial = "127.0.0.1:1935".parse().expect("initial address");
    let rotated = "127.0.0.2:1935".parse().expect("rotated address");
    let (resolver, queries) = test_resolver(
        initial,
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
        [],
        [Ok(vec![rotated])],
    );

    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Refreshed(rotated)
    );
    assert_eq!(resolver.address(), rotated);
    assert_eq!(resolver.refresh_if_due(), RtmpDnsRefresh::NotDue);
    assert_eq!(queries.calls(), 1);
}

#[test]
fn failed_dns_refresh_retains_the_last_address_and_retries_later() {
    let initial = "127.0.0.1:1935".parse().expect("initial address");
    let rotated = "127.0.0.2:1935".parse().expect("rotated address");
    let (resolver, queries) = test_resolver(
        initial,
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
        [],
        [Err(io::ErrorKind::NotFound), Ok(vec![rotated])],
    );

    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::Resolution)
    );
    assert_eq!(resolver.address(), initial);
    assert_eq!(resolver.refresh_if_due(), RtmpDnsRefresh::NotDue);
    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Refreshed(rotated)
    );
    assert_eq!(queries.calls(), 2);
}

#[test]
fn dns_refresh_rejects_a_family_mismatch_without_switching_addresses() {
    let initial = "127.0.0.1:1935".parse().expect("initial address");
    let (resolver, _) = test_resolver(
        initial,
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
        [],
        [Ok(vec!["[::1]:1935".parse().expect("IPv6 address")])],
    );

    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::FamilyMismatch)
    );
    assert_eq!(resolver.address(), initial);
}

#[test]
fn dns_refresh_rejects_a_direct_listener_loop_before_family_selection() {
    let initial = "127.0.0.2:1935".parse().expect("initial address");
    let listener = "127.0.0.1:1935".parse().expect("listener address");
    let (resolver, _) = test_resolver(
        initial,
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
        [listener],
        [Ok(vec![listener])],
    );

    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::DirectLoop)
    );
    assert_eq!(resolver.address(), initial);
}

#[test]
fn dns_refresh_rechecks_the_outbound_policy_for_new_answers() {
    let initial = "198.51.100.10:1935".parse().expect("initial address");
    let (resolver, _) = test_resolver(
        initial,
        RtmpOutboundPolicy::default(),
        [],
        [Ok(vec![
            "127.0.0.1:1935".parse().expect("loopback address"),
        ])],
    );

    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        resolver.refresh_if_due(),
        RtmpDnsRefresh::Failed(RtmpDnsRefreshFailure::Policy)
    );
    assert_eq!(resolver.address(), initial);
}

#[test]
fn overflow_and_codec_headers_gate_relay_output_until_a_matching_keyframe() {
    let controller = RtmpRelayController {
        shared: Arc::new(RelayShared {
            destination: RtmpDestination {
                address: "127.0.0.1:1935".parse().expect("destination"),
                host: "127.0.0.1".into(),
                transport: RtmpTransport::Rtmp,
                application: "live".into(),
                stream_name: "camera".into(),
                options: RtmpClientOptions::default(),
            },
            config: RtmpRelayConfig {
                max_queue_messages: 4,
                max_queue_bytes: 128,
                buffer_duration: Duration::from_secs(5),
                max_chain_depth: 4,
                ..RtmpRelayConfig::default()
            },
            resolver: None,
            state: Mutex::new(RelayState {
                accepting: true,
                waiting_for_keyframe: false,
                queue: VecDeque::new(),
                queue_bytes: 0,
                queue_started_at: None,
                cache: RelayCache::default(),
                phase: RtmpRelayPhase::Publishing,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_enqueued: 0,
                events_sent: 0,
                events_dropped: 0,
                payload_bytes_sent: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        }),
    };

    controller.try_enqueue(video(0, &[0x17, 0x00, 0, 0, 0, 0x01]));
    controller.try_enqueue(video(1, &[0x17, 0x01, 0, 0, 0, 0x11]));
    for timestamp in 2..=5 {
        controller.try_enqueue(video(timestamp, &[0x27, 0x01, 0, 0, 0, 0x22]));
    }
    controller
        .try_enqueue(MediaEvent::audio(6, Arc::<[u8]>::from([0xaf, 0x01, 0x33])).expect("audio"));
    controller.try_enqueue(video(7, &[0x27, 0x01, 0, 0, 0, 0x44]));
    {
        let state = controller.shared.lock();
        assert!(state.waiting_for_keyframe);
        assert!(state.queue.is_empty());
    }

    controller.try_enqueue(video(8, &[0x17, 0x00, 0, 0, 0, 0x02]));
    controller.try_enqueue(video(9, &[0x17, 0x01, 0, 0, 0, 0x55]));

    let state = controller.shared.lock();
    assert!(!state.waiting_for_keyframe);
    assert_eq!(state.queue.len(), 2);
    assert_eq!(state.queue[0].payload(), [0x17, 0x00, 0, 0, 0, 0x02]);
    assert_eq!(state.queue[1].payload(), [0x17, 0x01, 0, 0, 0, 0x55]);
}

#[test]
fn relay_queue_expires_after_the_configured_buffer_window() {
    let controller = RtmpRelayController {
        shared: Arc::new(RelayShared {
            destination: RtmpDestination {
                address: "127.0.0.1:1935".parse().expect("destination"),
                host: "127.0.0.1".into(),
                transport: RtmpTransport::Rtmp,
                application: "live".into(),
                stream_name: "camera".into(),
                options: RtmpClientOptions::default(),
            },
            config: RtmpRelayConfig {
                buffer_duration: Duration::from_millis(1),
                ..RtmpRelayConfig::default()
            },
            resolver: None,
            state: Mutex::new(RelayState {
                accepting: true,
                waiting_for_keyframe: false,
                queue: VecDeque::new(),
                queue_bytes: 0,
                queue_started_at: None,
                cache: RelayCache::default(),
                phase: RtmpRelayPhase::Publishing,
                last_failure: None,
                connection_attempts: 0,
                connections: 0,
                reconnects: 0,
                events_enqueued: 0,
                events_sent: 0,
                events_dropped: 0,
                payload_bytes_sent: 0,
                dns_refresh_attempts: 0,
                dns_refresh_successes: 0,
                dns_refresh_failures: 0,
                last_dns_refresh_failure: None,
            }),
            available: Condvar::new(),
        }),
    };
    controller.try_enqueue(video(0, &[0x17, 0x01, 0, 0, 0, 0x01]));
    thread::sleep(Duration::from_millis(5));
    assert!(controller.shared.next_event().is_none());
    let state = controller.shared.lock();
    assert_eq!(state.queue_bytes, 0);
    assert!(state.events_dropped > 0);
}

fn video(timestamp: u32, payload: &[u8]) -> MediaEvent {
    MediaEvent::video(timestamp, Arc::<[u8]>::from(payload)).expect("video event")
}
