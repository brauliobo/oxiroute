#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! TLS handshakes run on the handshake pool and the established streams are served by the listener's worker.

mod support;

use std::sync::Arc;

use oxiroute_config::{AlpnProtocol, HttpVersionPolicy};
use oxiroute_server::tls::handshake_pool_stats;
use tokio::time::timeout;

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, proxy_config, tls_client_config, tls_connect_with_config,
};

const CONNECTIONS: u64 = 24;

#[tokio::test]
async fn concurrent_handshakes_are_offloaded_and_every_connection_is_served() {
    timeout(TEST_TIMEOUT, async {
        let origin = PlainH1Origin::start(b"pool").await;
        let reserved = ReservedListener::new();
        let config = proxy_config(
            reserved.address,
            origin.address,
            vec![AlpnProtocol::Http11],
            None,
            HttpVersionPolicy::default(),
        );
        let proxy = ProxyHarness::start(&config.validate(), reserved);
        let client = tls_client_config(&fixture("ca-a.pem"), &[b"http/1.1"]).unwrap();
        let before = handshake_pool_stats();

        let connections: Vec<_> = (0..CONNECTIONS)
            .map(|_| {
                let client = Arc::clone(&client);
                let address = proxy.address;
                tokio::spawn(async move {
                    let mut stream = tls_connect_with_config(address, PROXY_SERVER_NAME, client)
                        .await
                        .expect("TLS connection to OxiRoute");
                    h1_request(&mut stream, "/pool", true).await.unwrap().status
                })
            })
            .collect();
        for connection in connections {
            assert_eq!(connection.await.unwrap(), 200);
        }

        let after = handshake_pool_stats();
        assert_eq!(after.started_total - before.started_total, CONNECTIONS);
        assert_eq!(after.queue_depth, 0);
        assert_eq!(after.rejected_total, before.rejected_total);
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("handshake pool wire test timed out");
}
