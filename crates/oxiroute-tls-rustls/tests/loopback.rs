use std::{future::poll_fn, mem::MaybeUninit, pin::Pin, task::Poll, time::Duration};

use pingora::protocols::tls::ALPN;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf},
    time::timeout,
};

mod common;

use common::{Fixed, Pair, connect, pair, pair_for};

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
async fn a_read_initializes_only_the_bytes_it_returns() {
    let Pair { client, server } = pair(&[b"http/1.1"]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (mut server, mut client) = (server.unwrap(), client.unwrap());
    client.write_all(b"ping").await.unwrap();
    client.flush().await.unwrap();

    let mut spare = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
    let mut buffer = ReadBuf::uninit(&mut spare);
    poll_fn(|context| Pin::new(&mut server).poll_read(context, &mut buffer))
        .await
        .unwrap();
    assert_eq!(buffer.filled(), b"ping");
    assert_eq!(buffer.initialized().len(), 4);
}

#[tokio::test]
async fn a_tls12_digest_names_the_suite_as_openssl_does() {
    let Pair { client, server } = pair_for(&[b"http/1.1"], &[&rustls::version::TLS12]);
    let (server, client) = connect(client, Fixed(Some(server))).await;
    let (server, _client) = (server.unwrap(), client.unwrap());

    let digest = server.get_ssl_digest().expect("TLS digest");
    assert_eq!(digest.version, "TLSv1.2");
    assert!(
        digest.cipher.starts_with("ECDHE-ECDSA-"),
        "{}",
        digest.cipher
    );
    assert!(!digest.cipher.contains('_'), "{}", digest.cipher);
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
