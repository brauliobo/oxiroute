#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]
//! TLS handshakes run on the offload threads and the established streams are served by the listener's worker.

mod support;

use std::sync::Arc;

use oxiroute_config::{AlpnProtocol, HttpVersionPolicy};
use tokio::time::timeout;

use support::{
    PROXY_SERVER_NAME, PlainH1Origin, ProxyHarness, ReservedListener, TEST_TIMEOUT, fixture,
    h1_request, proxy_config, tls_client_config, tls_connect_with_config,
};

const CONNECTIONS: u64 = 24;

fn thread_names() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("task list")
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .map(|name| name.trim().to_owned())
        .collect()
}

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

        assert!(
            thread_names()
                .iter()
                .any(|name| name.starts_with("downstream TLS")),
            "no offload thread ran the handshakes"
        );
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("handshake pool wire test timed out");
}
