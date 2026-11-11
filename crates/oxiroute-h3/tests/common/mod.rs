//! Loopback fixtures: a self-signed endpoint pair on ports the OS assigned (the sockets stay bound, nothing is
//! picked and released), an `h3` crate client, and a raw quinn client.
#![allow(dead_code)]
pub mod raw;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use oxiroute_h3::server::{Config, Connection, RequestStream};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::RootCertStore;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub const TIMEOUT: Duration = Duration::from_secs(30);

pub type H3Client = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

pub struct Pki {
    cert: CertificateDer<'static>,
    key: Vec<u8>,
}

pub fn pki() -> Pki {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("certificate");
    Pki {
        cert: certified.cert.der().clone(),
        key: certified.signing_key.serialize_der(),
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn server_config(pki: &Pki, streams: u32) -> quinn::ServerConfig {
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![pki.cert.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pki.key.clone())),
        )
        .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(streams.into());
    transport.max_concurrent_uni_streams(32u32.into());
    config.transport_config(Arc::new(transport));
    config
}

pub fn client_config(pki: &Pki) -> quinn::ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.add(pki.cert.clone()).unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).unwrap()))
}

pub struct Fixture {
    pub server: quinn::Endpoint,
    pub client: quinn::Endpoint,
    pub addr: SocketAddr,
}

pub fn fixture() -> Fixture {
    fixture_with_streams(256)
}

pub fn fixture_with_streams(streams: u32) -> Fixture {
    let pki = pki();
    let server =
        quinn::Endpoint::server(server_config(&pki, streams), "127.0.0.1:0".parse().unwrap())
            .unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(client_config(&pki));
    Fixture {
        server,
        client,
        addr,
    }
}

impl Fixture {
    /// Connects a client and accepts the connection on the server: `(client side, server side)`.
    pub async fn connect(&self) -> (quinn::Connection, quinn::Connection) {
        let connecting = self.client.connect(self.addr, "localhost").unwrap();
        let accepting = async { self.server.accept().await.unwrap().await };
        let (client, server) = tokio::join!(connecting, accepting);
        (client.unwrap(), server.unwrap())
    }

    /// Serves every connection with `handler` per request, each request on its own task, until the endpoint closes.
    pub fn serve<H, F>(&self, config: Config, handler: H)
    where
        H: Fn(RequestStream) -> F + Clone + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let server = self.server.clone();
        tokio::spawn(async move {
            while let Some(incoming) = server.accept().await {
                let (config, handler) = (config.clone(), handler.clone());
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    serve_connection(conn, config, handler).await;
                });
            }
        });
    }

    /// An `h3` crate client connected to the server of this fixture.
    pub async fn h3_client(&self) -> (H3Client, quinn::Connection) {
        let conn = self
            .client
            .connect(self.addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(conn.clone()))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });
        (send, conn)
    }
}

pub async fn serve_connection<H, F>(conn: quinn::Connection, config: Config, handler: H)
where
    H: Fn(RequestStream) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let mut h3 = Connection::new(conn, config).await.unwrap();
    while let Ok(Some(stream)) = h3.accept().await {
        tokio::spawn(handler.clone()(stream));
    }
}

pub async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}
