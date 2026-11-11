//! A client that resets requests in a flood loses its connection; ordinary cancellations do not.
use super::raw::{open_control, raw_request};
#[allow(clippy::wildcard_imports)]
use super::*;

const FLOOD_REQUESTS: usize = 1_500;
const H3_EXCESSIVE_LOAD: u32 = 0x107;

/// Opens a request stream, sends its head and resets both directions at once.
async fn reset_request(connection: &quinn::Connection) -> Result<(), quinn::ConnectionError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|_| connection_error(connection))?;
    send.write_all(&raw_request(&Method::GET, "/")).await.ok();
    let cancelled = quinn::VarInt::from_u32(0x10c);
    send.reset(cancelled).ok();
    recv.stop(cancelled).ok();
    Ok(())
}

fn connection_error(connection: &quinn::Connection) -> quinn::ConnectionError {
    connection
        .close_reason()
        .expect("a closed connection has a reason")
}

#[tokio::test]
pub(super) async fn reverse_h3_closes_a_connection_that_resets_a_flood_of_requests() {
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(
        listener_address,
        key.path(),
        fixed_service(),
        Vec::new(),
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");

    let calm = connect_h3(&endpoint, listener_address).await;
    let _calm_control = open_control(&calm).await;
    for _ in 0..20 {
        reset_request(&calm)
            .await
            .expect("a few cancellations are fine");
    }
    sleep(Duration::from_millis(200)).await;
    assert!(
        calm.close_reason().is_none(),
        "a handful of cancelled requests closed the connection"
    );

    let flooding = connect_h3(&endpoint, listener_address).await;
    let _control = open_control(&flooding).await;
    for _ in 0..FLOOD_REQUESTS {
        if reset_request(&flooding).await.is_err() {
            break;
        }
    }
    let close = timeout(Duration::from_secs(30), flooding.closed())
        .await
        .expect("the flooding connection stayed open");
    assert!(
        matches!(&close, quinn::ConnectionError::ApplicationClosed(reason)
            if reason.error_code == quinn::VarInt::from_u32(H3_EXCESSIVE_LOAD)),
        "unexpected close: {close:?}"
    );
    assert!(
        calm.close_reason().is_none(),
        "the calm connection was closed too"
    );

    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    server.shutdown_gracefully();
}
