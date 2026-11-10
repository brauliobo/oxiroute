use std::{future::poll_fn, net::Ipv4Addr, sync::Arc, task::Poll, time::Duration};

use oxiroute_tls_rustls::{ConfigRouter, RustlsAcceptor};
use pingora::{
    listeners::StreamTlsAcceptor,
    protocols::{Stream, l4::stream::Stream as L4Stream, tls::ALPN},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    crypto::ring,
    pki_types::{PrivatePkcs8KeyDer, ServerName},
    server::ClientHello,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_rustls::TlsConnector;

const NAME: &str = "localhost";

struct Fixed(Option<Arc<ServerConfig>>);

impl ConfigRouter for Fixed {
    fn route(&self, _hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        self.0.clone()
    }
}

struct Pair {
    client: ClientConfig,
    server: Arc<ServerConfig>,
}

fn pair(alpn: &[&[u8]]) -> Pair {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![NAME.into()]).unwrap();
    let provider = Arc::new(ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
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
        .with_safe_default_protocol_versions()
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
async fn connect(
    client: ClientConfig,
    router: Fixed,
) -> (
    pingora::Result<Stream>,
    std::io::Result<tokio_rustls::client::TlsStream<TcpStream>>,
) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = RustlsAcceptor::new(Arc::new(router));
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        acceptor.accept(L4Stream::from(socket)).await
    });
    let socket = TcpStream::connect(address).await.unwrap();
    let client = TlsConnector::from(Arc::new(client))
        .connect(ServerName::try_from(NAME).unwrap(), socket)
        .await;
    (server.await.unwrap(), client)
}

#[tokio::test]
async fn serves_requests_and_reports_what_was_negotiated() {
    let Pair { client, server } = pair(&[b"h2", b"http/1.1"]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (mut server, mut client) = (server.unwrap(), client.unwrap());

    let digest = server.get_ssl_digest().expect("TLS digest");
    assert_eq!(digest.version, "TLSv1.3");
    assert!(digest.cipher.starts_with("TLS_AES_"), "{}", digest.cipher);
    assert_eq!(server.selected_alpn_proto(), Some(ALPN::H2));
    assert_eq!(client.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));

    client.write_all(b"ping").await.unwrap();
    let mut request = [0; 4];
    server.read_exact(&mut request).await.unwrap();
    assert_eq!(&request, b"ping");

    server.write_all(b"po").await.unwrap();
    server.write_all(b"ng").await.unwrap();
    server.flush().await.unwrap();
    let mut response = [0; 4];
    client.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"pong");

    AsyncWriteExt::shutdown(&mut server).await.unwrap();
    assert_eq!(client.read(&mut response).await.unwrap(), 0);
}

#[tokio::test]
async fn large_and_vectored_writes_arrive_whole_and_in_order() {
    let Pair { client, server } = pair(&[b"http/1.1"]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (mut server, mut client) = (server.unwrap(), client.unwrap());

    let body: Vec<u8> = (0..200_000u32).map(|byte| (byte % 251) as u8).collect();
    let sent = body.clone();
    let writer = tokio::spawn(async move {
        server.write_all(&sent[..100]).await.unwrap();
        let vectored = [
            std::io::IoSlice::new(&sent[100..109]),
            std::io::IoSlice::new(&sent[109..4_000]),
        ];
        let taken = 100 + server.write_vectored(&vectored).await.unwrap();
        server.write_all(&sent[taken..]).await.unwrap();
        server.flush().await.unwrap();
        server
    });
    let mut received = vec![0; body.len()];
    timeout(Duration::from_secs(10), client.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    writer.await.unwrap();
    assert!(received == body, "the bytes changed on the way");
}

#[tokio::test]
async fn a_peer_that_closes_without_close_notify_ends_the_stream() {
    let Pair { client, server } = pair(&[]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (mut server, mut client) = (server.unwrap(), client.unwrap());

    AsyncWriteExt::shutdown(&mut client.get_mut().0)
        .await
        .unwrap();
    let mut buffer = [0; 8];
    assert_eq!(server.read(&mut buffer).await.unwrap(), 0);
}

#[tokio::test]
async fn readiness_waits_for_the_peer_without_decrypting() {
    let Pair { client, server } = pair(&[]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (mut server, mut client) = (server.unwrap(), client.unwrap());

    let idle = poll_fn(|context| Poll::Ready(server.poll_read_ready(context))).await;
    assert!(idle.is_pending(), "nothing was sent yet");

    client.write_all(b"x").await.unwrap();
    client.flush().await.unwrap();
    timeout(
        Duration::from_secs(5),
        poll_fn(|context| server.poll_read_ready(context)),
    )
    .await
    .unwrap()
    .unwrap();
    let mut byte = [0; 1];
    server.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"x");
}

#[tokio::test]
async fn a_refused_hello_fails_both_sides() {
    let Pair { client, .. } = pair(&[b"h2"]);
    let (server, client) = connect(client, Fixed(None)).await;

    assert!(server.is_err());
    assert!(client.is_err());
}
