use super::raw::{open_control, raw_request};
#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn reverse_h3_stops_an_early_answered_request_with_no_error() {
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
    let connection = connect_h3(&endpoint, listener_address).await;
    let _control = open_control(&connection).await;

    let (mut send, mut recv) = connection.open_bi().await.expect("request stream");
    send.write_all(&raw_request(&Method::GET, "/"))
        .await
        .expect("request head");
    let response = recv.read_to_end(1 << 20).await.expect("complete response");
    assert!(
        !response.is_empty(),
        "the unfinished request was not answered"
    );
    assert_eq!(
        send.stopped().await.expect("request stream stop"),
        Some(quinn::VarInt::from_u32(0x100))
    );

    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    server.shutdown_gracefully();
}
