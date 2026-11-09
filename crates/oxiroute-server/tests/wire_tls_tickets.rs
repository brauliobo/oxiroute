#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! A TLS 1.3 server that cannot resume a session sends no session ticket.

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use oxiroute_config::{AlpnProtocol, HttpVersionPolicy};
use rustls::{
    NamedGroup,
    client::{ClientSessionStore, Resumption, Tls12ClientSessionValue, Tls13ClientSessionValue},
    pki_types::ServerName,
};
use tokio::time::timeout;

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, proxy_config, tls_client_config, tls_connect_with_config,
};

/// Counts the TLS 1.3 tickets a client receives.
#[derive(Debug, Default)]
struct TicketCounter(AtomicUsize);

impl ClientSessionStore for TicketCounter {
    fn set_kx_hint(&self, _: ServerName<'static>, _: NamedGroup) {}
    fn kx_hint(&self, _: &ServerName<'_>) -> Option<NamedGroup> {
        None
    }
    fn set_tls12_session(&self, _: ServerName<'static>, _: Tls12ClientSessionValue) {}
    fn tls12_session(&self, _: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
        None
    }
    fn remove_tls12_session(&self, _: &ServerName<'static>) {}
    fn insert_tls13_ticket(&self, _: ServerName<'static>, _: Tls13ClientSessionValue) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn take_tls13_ticket(&self, _: &ServerName<'static>) -> Option<Tls13ClientSessionValue> {
        None
    }
}

async fn tickets_received(session_tickets: bool) -> usize {
    let origin = PlainH1Origin::start(b"tickets").await;
    let reserved = ReservedListener::new();
    let mut config = proxy_config(
        reserved.address,
        origin.address,
        vec![AlpnProtocol::Http11],
        None,
        HttpVersionPolicy::default(),
    );
    config.tls_profiles[0].policy.session_tickets = session_tickets;
    let proxy = ProxyHarness::start(&config.validate(), reserved);

    let counter = Arc::new(TicketCounter::default());
    let mut client = (*tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"]).unwrap()).clone();
    client.resumption = Resumption::store(counter.clone());
    let mut stream = tls_connect_with_config(proxy.address, PROXY_SERVER_NAME, Arc::new(client))
        .await
        .expect("TLS connection to OxiRoute");
    // Tickets follow the handshake; the client processes them while it reads the response.
    let response = h1_request(&mut stream, "/tickets", true).await.unwrap();
    assert_eq!(response.status, 200);

    drop(stream);
    proxy.finish().await;
    origin.finish().await;
    counter.0.load(Ordering::Relaxed)
}

#[tokio::test]
async fn no_tickets_are_sent_when_nothing_can_resume_and_stateless_tickets_still_are() {
    timeout(TEST_TIMEOUT, async {
        assert_eq!(tickets_received(false).await, 0);
        assert!(
            tickets_received(true).await > 0,
            "the counter sees the tickets of a profile that enables them"
        );
    })
    .await
    .expect("ticket wire test timed out");
}
