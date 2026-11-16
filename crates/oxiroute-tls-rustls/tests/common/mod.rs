#![allow(dead_code)]

use std::{net::Ipv4Addr, sync::Arc};

use oxiroute_tls_rustls::{ConfigRouter, RustlsAcceptor};
use pingora::{
    listeners::StreamTlsAcceptor,
    protocols::{Stream, l4::stream::Stream as L4Stream},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    crypto::{CryptoProvider, ring},
    pki_types::{PrivatePkcs8KeyDer, ServerName},
    server::ClientHello,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

pub const NAME: &str = "localhost";

pub struct Fixed(pub Option<Arc<ServerConfig>>);

impl ConfigRouter for Fixed {
    fn route(&self, _hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        self.0.clone()
    }
}

pub struct Pair {
    pub client: ClientConfig,
    pub server: Arc<ServerConfig>,
}

pub fn pair(alpn: &[&[u8]]) -> Pair {
    pair_for(alpn, rustls::ALL_VERSIONS)
}

pub fn pair_for(alpn: &[&[u8]], versions: &[&'static rustls::SupportedProtocolVersion]) -> Pair {
    pair_with(alpn, versions, Arc::new(ring::default_provider()))
}

pub fn pair_with(
    alpn: &[&[u8]],
    versions: &[&'static rustls::SupportedProtocolVersion],
    provider: Arc<CryptoProvider>,
) -> Pair {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![NAME.into()]).unwrap();
    let mut server = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
    server.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    let mut roots = RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let mut client = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    Pair {
        client,
        server: Arc::new(server),
    }
}

/// Accepts one connection with `router` and returns what the server side made of it next to the connected client.
pub async fn connect(
    client: ClientConfig,
    router: Fixed,
) -> (
    pingora::Result<Stream>,
    std::io::Result<tokio_rustls::client::TlsStream<TcpStream>>,
) {
    connect_with(Arc::new(client), RustlsAcceptor::new(Arc::new(router))).await
}

pub async fn connect_with(
    client: Arc<ClientConfig>,
    acceptor: RustlsAcceptor,
) -> (
    pingora::Result<Stream>,
    std::io::Result<tokio_rustls::client::TlsStream<TcpStream>>,
) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        acceptor.accept(L4Stream::from(socket)).await
    });
    let socket = TcpStream::connect(address).await.unwrap();
    let client = TlsConnector::from(client)
        .connect(ServerName::try_from(NAME).unwrap(), socket)
        .await;
    (server.await.unwrap(), client)
}
