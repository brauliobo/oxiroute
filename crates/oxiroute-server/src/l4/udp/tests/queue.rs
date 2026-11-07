#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
async fn queue_enforces_datagram_and_byte_bounds() {
    let (queue, mut receiver) = DatagramQueue::new(policy()).expect("queue");
    assert!(matches!(
        queue.try_send(vec![0; 8]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(
        queue.try_send(vec![0; 4]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));
    let datagram = receiver.recv().await.expect("queued datagram");
    assert_eq!(datagram.payload.len(), 8);
    drop(datagram);
    assert!(matches!(
        queue.try_send(vec![0; 4]),
        QueueSendResult::Enqueued
    ));
}

#[tokio::test]
async fn queue_rejects_datagrams_at_the_count_bound() {
    let mut policy = policy();
    policy.max_queue_datagrams = 2;
    policy.max_queue_bytes = 64;
    let (queue, mut receiver) = DatagramQueue::new(policy).expect("queue");

    assert!(matches!(
        queue.try_send(vec![0; 1]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(
        queue.try_send(vec![0; 1]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));

    drop(receiver.recv().await.expect("first queued datagram"));
    assert!(matches!(
        queue.try_send(vec![0; 1]),
        QueueSendResult::Enqueued
    ));
}

#[tokio::test]
async fn queue_rejects_datagrams_at_the_byte_bound() {
    let mut policy = policy();
    policy.max_queue_datagrams = 4;
    policy.max_queue_bytes = 5;
    let (queue, mut receiver) = DatagramQueue::new(policy).expect("queue");

    assert!(matches!(
        queue.try_send(vec![0; 3]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(
        queue.try_send(vec![0; 2]),
        QueueSendResult::Enqueued
    ));
    assert!(matches!(queue.try_send(vec![0; 1]), QueueSendResult::Full));

    drop(receiver.recv().await.expect("first queued datagram"));
    assert!(matches!(
        queue.try_send(vec![0; 1]),
        QueueSendResult::Enqueued
    ));
}

#[tokio::test]
async fn queue_reports_closed_session_without_retaining_bytes() {
    let (queue, receiver) = DatagramQueue::new(policy()).expect("queue");
    drop(receiver);
    assert!(matches!(
        queue.try_send(vec![0; 4]),
        QueueSendResult::Closed
    ));
    assert_eq!(queue.queued_bytes.load(Ordering::Acquire), 0);
}

#[test]
fn session_byte_accounting_rejects_only_bytes_after_the_bound() {
    let mut total = 0;
    assert!(account_session_bytes(&mut total, 64, policy()).is_ok());
    assert_eq!(total, 64);
    assert!(matches!(
        account_session_bytes(&mut total, 1, policy()),
        Err(SessionEnd::SessionBytesLimit)
    ));
    assert_eq!(total, 64);
}

#[test]
fn session_failure_attribution_only_observes_genuine_upstream_outcomes() {
    assert_eq!(
        SessionEnd::UpstreamConnect(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"))
            .passive_failure(),
        Some(HealthFailure::ConnectFailed)
    );
    assert_eq!(
        SessionEnd::UpstreamSend(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"))
            .passive_failure(),
        Some(HealthFailure::ProtocolError)
    );
    assert_eq!(
        SessionEnd::UpstreamReceive(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
            .passive_failure(),
        Some(HealthFailure::Timeout)
    );
    assert_eq!(
        SessionEnd::UpstreamProtocol(io::Error::new(io::ErrorKind::InvalidData, "oversized"))
            .passive_failure(),
        Some(HealthFailure::ProtocolError)
    );
    assert_eq!(
        SessionEnd::UpstreamProxyProtocol(ProxyProtocolError::new(
            ProxyProtocolErrorKind::Cancelled,
        ))
        .passive_failure(),
        None
    );

    let excluded = [
        SessionEnd::Cancelled,
        SessionEnd::IdleTimeout,
        SessionEnd::LifetimeTimeout,
        SessionEnd::Connect(io::Error::other("pool unavailable")),
        SessionEnd::ClientSend(io::Error::other("client closed")),
        SessionEnd::ProxyProtocol(ProxyProtocolError::new(
            ProxyProtocolErrorKind::InvalidLength,
        )),
        SessionEnd::Accounting,
        SessionEnd::SessionBytesLimit,
    ];
    assert!(
        excluded
            .iter()
            .all(|outcome| outcome.passive_failure().is_none())
    );
}
