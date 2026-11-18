use oxiroute_config::{AlpnProtocol, HttpVersionPolicy, TlsBackend};
use rustls::HandshakeKind;

use super::*;
use process_support::reserve_tcp_address;
use wire_support::{
    PROXY_SERVER_NAME, fixture, h1_request, handshake_kind, proxy_config, tls_client_config,
    tls_connect_with_config,
};

async fn handshake(
    listener: SocketAddr,
    client: &Arc<rustls::ClientConfig>,
) -> Option<HandshakeKind> {
    let mut stream = tls_connect_with_config(listener, PROXY_SERVER_NAME, Arc::clone(client))
        .await
        .expect("TLS connection");
    let kind = handshake_kind(&stream);
    let response = h1_request(&mut stream, "/", true).await.expect("response");
    assert_eq!(response.status, 200);
    kind
}

async fn a_ticket_issued_before_a_reload_resumes_after_it(supervised: bool) {
    let origin = spawn_origin();
    let (management, listener) = (reserve_tcp_address(), reserve_tcp_address());
    let mut proxy = proxy_config(
        listener,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    proxy.management = Some(super::management(management));
    proxy.tls_profiles[0].policy.backend = Some(TlsBackend::Rustls);
    proxy.tls_profiles[0].policy.session_tickets = true;
    let mut draft = (*proxy).clone();
    let validated = proxy.validate();
    let mut server = start(&validated, supervised);
    server.wait_for_tcp(management).await;
    server.wait_for_tcp(listener).await;
    let client = tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"]).expect("client");

    assert_eq!(
        handshake(listener, &client).await,
        Some(HandshakeKind::Full)
    );
    assert_eq!(
        handshake(listener, &client).await,
        Some(HandshakeKind::Resumed),
        "the profile does not resume sessions before the reload"
    );

    reload(&server, management, &mut draft, 1).await;

    assert_eq!(
        handshake(listener, &client).await,
        Some(HandshakeKind::Resumed),
        "a ticket issued before the reload did not resume after it"
    );
    server.shutdown();
}

#[tokio::test]
async fn a_direct_reload_keeps_tls_session_tickets_valid() {
    a_ticket_issued_before_a_reload_resumes_after_it(false).await;
}

#[tokio::test]
async fn a_supervised_reload_keeps_tls_session_tickets_valid() {
    a_ticket_issued_before_a_reload_resumes_after_it(true).await;
}
