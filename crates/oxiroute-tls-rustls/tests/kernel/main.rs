#![cfg(target_os = "linux")]
//! Connections converted to kernel TLS over real sockets: every suite, both directions, resumption, conversion while
//! the peer is still sending, `close_notify`, `KeyUpdate`, and the connections that stay in user space.
//!
//! The kernel's `tls` module has to be loaded (`sudo modprobe tls`); without it the tests that need the kernel say
//! so and pass.

use std::{future::poll_fn, io::Write, sync::Arc, time::Duration};

use oxiroute_tls_rustls::{KernelTls, RustlsAcceptor, probe};
use pingora::protocols::{
    Stream,
    l4::sendfile::{send_file, send_file_to_stream},
};
use rustls::{
    HandshakeKind, SupportedCipherSuite, SupportedProtocolVersion,
    crypto::{CryptoProvider, aws_lc_rs},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

#[path = "../common/mod.rs"]
mod common;

use common::{Fixed, Pair, connect_with, pair_with};

type Client = tokio_rustls::client::TlsStream<TcpStream>;

const BODY: usize = 3 * 1024 * 1024 + 17;
const TIMEOUT: Duration = Duration::from_secs(20);

struct Case {
    name: &'static str,
    version: &'static SupportedProtocolVersion,
    suite: SupportedCipherSuite,
}

fn cases() -> [Case; 6] {
    use aws_lc_rs::cipher_suite::*;
    [
        Case {
            name: "TLS 1.3 AES-128-GCM",
            version: &rustls::version::TLS13,
            suite: TLS13_AES_128_GCM_SHA256,
        },
        Case {
            name: "TLS 1.3 AES-256-GCM",
            version: &rustls::version::TLS13,
            suite: TLS13_AES_256_GCM_SHA384,
        },
        Case {
            name: "TLS 1.3 ChaCha20-Poly1305",
            version: &rustls::version::TLS13,
            suite: TLS13_CHACHA20_POLY1305_SHA256,
        },
        Case {
            name: "TLS 1.2 AES-128-GCM",
            version: &rustls::version::TLS12,
            suite: TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        },
        Case {
            name: "TLS 1.2 AES-256-GCM",
            version: &rustls::version::TLS12,
            suite: TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        },
        Case {
            name: "TLS 1.2 ChaCha20-Poly1305",
            version: &rustls::version::TLS12,
            suite: TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
        },
    ]
}

fn tls13_aes() -> Case {
    let [case, ..] = cases();
    case
}

fn tls12_aes() -> Case {
    let [_, _, _, case, ..] = cases();
    case
}

fn kernel() -> Option<KernelTls> {
    match probe() {
        Ok(kernel) => Some(kernel),
        Err(unavailable) => {
            eprintln!("skipped: {unavailable}");
            None
        }
    }
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from((index * 7 + index / 251) % 251).expect("below 251"))
        .collect()
}

fn pair_of(case: &Case, extraction: bool) -> Pair {
    let provider = Arc::new(CryptoProvider {
        cipher_suites: vec![case.suite],
        ..aws_lc_rs::default_provider()
    });
    let mut pair = pair_with(&[b"http/1.1"], &[case.version], provider);
    let mut server = (*pair.server).clone();
    server.enable_secret_extraction = extraction;
    pair.server = Arc::new(server);
    pair
}

fn acceptor(pair: &Pair, kernel: Option<KernelTls>) -> RustlsAcceptor {
    let acceptor = RustlsAcceptor::new(Arc::new(Fixed(Some(Arc::clone(&pair.server)))));
    match kernel {
        Some(kernel) => acceptor.with_kernel_tls(kernel),
        None => acceptor,
    }
}

async fn serve(case: &Case, extraction: bool, kernel: Option<KernelTls>) -> (Stream, Client) {
    let pair = pair_of(case, extraction);
    let (server, client) =
        connect_with(Arc::new(pair.client.clone()), acceptor(&pair, kernel)).await;
    (server.unwrap(), client.unwrap())
}

async fn offload(server: &mut Stream) -> bool {
    poll_fn(|context| server.poll_kernel_tx(context)).await
}

async fn push<W, R>(from: &mut W, to: &mut R, body: &[u8])
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut received = vec![0; body.len()];
    let (written, read) = tokio::join!(
        async {
            from.write_all(body).await?;
            from.flush().await
        },
        timeout(TIMEOUT, to.read_exact(&mut received))
    );
    written.unwrap();
    read.expect("the body did not arrive in time").unwrap();
    assert!(received == body, "the bytes changed on the way");
}

async fn talk_in_user_space(server: &mut Stream, client: &mut Client) {
    push(client, server, b"GET /big HTTP/1.1\r\n\r\n").await;
    push(server, client, b"HTTP/1.1 200 OK\r\n\r\n").await;
}

async fn move_large_bodies(server: &mut Stream, client: &mut Client) {
    let body = pattern(BODY);
    push(server, client, &body).await;
    push(client, server, &body).await;
    push(server, client, b"tail").await;
}

fn tx_sockets() -> u64 {
    std::fs::read_to_string("/proc/net/tls_stat")
        .ok()
        .and_then(|stat| {
            stat.lines()
                .find_map(|line| line.strip_prefix("TlsTxSw"))
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn every_suite_moves_large_bodies_both_ways_after_the_conversion() {
    let Some(kernel) = kernel() else { return };
    for case in cases() {
        let before = tx_sockets();
        let (mut server, mut client) = serve(&case, true, Some(kernel)).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await, "{}: not converted", case.name);
        assert!(server.kernel_tx_socket().is_some(), "{}", case.name);
        assert!(
            tx_sockets() > before,
            "{}: the kernel did not encrypt",
            case.name
        );
        move_large_bodies(&mut server, &mut client).await;
        move_large_bodies(&mut server, &mut client).await;
    }
}

#[tokio::test]
async fn a_file_sent_with_sendfile_arrives_intact_between_ordinary_writes() {
    let Some(kernel) = kernel() else { return };
    for case in [tls13_aes(), tls12_aes()] {
        let body = pattern(BODY);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&body).unwrap();
        let (mut server, mut client) = serve(&case, true, Some(kernel)).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await, "{}", case.name);

        push(&mut server, &mut client, b"head").await;
        let socket = server.kernel_tx_socket().unwrap();
        let mut received = vec![0; body.len()];
        let (sent, read) = tokio::join!(
            send_file(socket, &file, 0, body.len()),
            timeout(TIMEOUT, client.read_exact(&mut received))
        );
        assert_eq!(sent.unwrap(), body.len(), "{}", case.name);
        read.unwrap().unwrap();
        assert!(
            received == body,
            "{}: the file changed on the way",
            case.name
        );
        push(&mut server, &mut client, b"tail").await;
        push(&mut client, &mut server, b"next request").await;
    }
}

#[tokio::test]
async fn a_bare_stream_sends_a_file_by_converting_to_kernel_tls() {
    let Some(kernel) = kernel() else { return };
    let body = pattern(BODY);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&body).unwrap();
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    talk_in_user_space(&mut server, &mut client).await;

    let mut received = vec![0; body.len()];
    let (sent, read) = tokio::join!(
        send_file_to_stream(&mut server, &file, 0, body.len()),
        timeout(TIMEOUT, client.read_exact(&mut received))
    );
    assert_eq!(sent.unwrap(), Some(body.len()));
    read.unwrap().unwrap();
    assert!(received == body, "the file changed on the way");
    assert!(server.kernel_tx_socket().is_some());
    push(&mut client, &mut server, b"next request").await;
}

#[tokio::test]
async fn a_bare_stream_that_cannot_convert_is_given_no_file() {
    let file = tempfile::tempfile().unwrap();
    for (offered, extraction) in [(None, true), (kernel(), false)] {
        let (mut server, mut client) = serve(&tls13_aes(), extraction, offered).await;
        talk_in_user_space(&mut server, &mut client).await;
        let sent = send_file_to_stream(&mut server, &file, 0, 1).await;
        assert_eq!(sent.unwrap(), None);
    }
}

#[tokio::test]
async fn a_server_close_after_the_conversion_ends_with_close_notify() {
    let Some(kernel) = kernel() else { return };
    for case in [tls13_aes(), tls12_aes()] {
        let (mut server, mut client) = serve(&case, true, Some(kernel)).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await);
        push(&mut server, &mut client, b"last bytes").await;
        AsyncWriteExt::shutdown(&mut server).await.unwrap();

        let mut rest = Vec::new();
        timeout(TIMEOUT, client.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("{}: not a clean close: {error}", case.name));
        assert_eq!(rest.len(), 0);
    }
}

#[tokio::test]
async fn a_close_without_close_notify_is_not_mistaken_for_a_clean_one() {
    let Some(kernel) = kernel() else { return };
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    talk_in_user_space(&mut server, &mut client).await;
    assert!(offload(&mut server).await);
    drop(server);

    let error = timeout(TIMEOUT, client.read_to_end(&mut Vec::new()))
        .await
        .unwrap()
        .expect_err("a truncated stream");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn a_client_close_after_the_conversion_ends_the_server_read_cleanly() {
    let Some(kernel) = kernel() else { return };
    for case in [tls13_aes(), tls12_aes()] {
        let (mut server, mut client) = serve(&case, true, Some(kernel)).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await);
        push(&mut client, &mut server, b"request").await;
        AsyncWriteExt::shutdown(&mut client).await.unwrap();

        let mut rest = Vec::new();
        timeout(TIMEOUT, server.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(rest.len(), 0);
    }
}

async fn resumed_connection(case: &Case, kernel: KernelTls) {
    let mut pair = pair_of(case, true);
    let mut server = (*pair.server).clone();
    server.ticketer = aws_lc_rs::Ticketer::new().unwrap();
    pair.server = Arc::new(server);
    let client = Arc::new(pair.client.clone());

    let (first, first_client) =
        connect_with(Arc::clone(&client), acceptor(&pair, Some(kernel))).await;
    let (mut first, mut first_client) = (first.unwrap(), first_client.unwrap());
    assert_eq!(
        first_client.get_ref().1.handshake_kind(),
        Some(HandshakeKind::Full)
    );
    push(&mut first, &mut first_client, b"tickets travel with data").await;
    drop((first, first_client));

    let (second, second_client) = connect_with(client, acceptor(&pair, Some(kernel))).await;
    let (mut second, mut second_client) = (second.unwrap(), second_client.unwrap());
    assert_eq!(
        second_client.get_ref().1.handshake_kind(),
        Some(HandshakeKind::Resumed),
        "{}",
        case.name
    );
    talk_in_user_space(&mut second, &mut second_client).await;
    assert!(offload(&mut second).await, "{}", case.name);
    move_large_bodies(&mut second, &mut second_client).await;
}

#[tokio::test]
async fn resumed_connections_are_converted_like_full_ones() {
    let Some(kernel) = kernel() else { return };
    resumed_connection(&tls13_aes(), kernel).await;
    resumed_connection(&tls12_aes(), kernel).await;
}

#[tokio::test]
async fn keys_rotated_before_the_conversion_are_the_keys_the_kernel_gets() {
    let Some(kernel) = kernel() else { return };
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    talk_in_user_space(&mut server, &mut client).await;
    client.get_mut().1.refresh_traffic_keys().unwrap();
    push(&mut client, &mut server, b"after the key update").await;
    push(&mut server, &mut client, b"answered with new keys").await;

    assert!(offload(&mut server).await);
    move_large_bodies(&mut server, &mut client).await;
}

#[tokio::test]
async fn a_key_update_after_the_conversion_ends_the_connection() {
    let Some(kernel) = kernel() else { return };
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    talk_in_user_space(&mut server, &mut client).await;
    assert!(offload(&mut server).await);

    client.get_mut().1.refresh_traffic_keys().unwrap();
    client.write_all(b"after the key update").await.unwrap();
    client.flush().await.unwrap();
    let error = timeout(TIMEOUT, async {
        let mut buffer = [0; 64];
        loop {
            if let Err(error) = server.read(&mut buffer).await {
                break error;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[tokio::test]
async fn converting_while_the_peer_is_still_sending_loses_nothing() {
    let Some(kernel) = kernel() else { return };
    for case in [tls13_aes(), tls12_aes()] {
        for (length, prefix) in [
            (1, 1),
            (100, 7),
            (16_384, 16_384),
            (16_385, 16_384),
            (40_000, 16_384),
            (40_000, 1),
            (200_000, 65_536),
            (BODY, 100_000),
        ] {
            let (mut server, mut client) = serve(&case, true, Some(kernel)).await;
            talk_in_user_space(&mut server, &mut client).await;
            let body = pattern(length);
            let sent = body.clone();
            let writer = tokio::spawn(async move {
                client.write_all(&sent).await.unwrap();
                client.flush().await.unwrap();
                client
            });
            let mut received = vec![0; length];
            timeout(TIMEOUT, server.read_exact(&mut received[..prefix]))
                .await
                .unwrap()
                .unwrap();
            offload(&mut server).await;
            timeout(TIMEOUT, server.read_exact(&mut received[prefix..]))
                .await
                .unwrap()
                .unwrap();
            assert!(received == body, "{} {length}/{prefix}", case.name);
            let mut client = writer.await.unwrap();
            push(&mut server, &mut client, b"done").await;
        }
    }
}

#[tokio::test]
async fn plaintext_the_server_has_not_read_keeps_the_connection_in_user_space() {
    let Some(kernel) = kernel() else { return };
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    client.write_all(b"abc").await.unwrap();
    client.flush().await.unwrap();
    let mut first = [0; 1];
    timeout(TIMEOUT, server.read_exact(&mut first))
        .await
        .unwrap()
        .unwrap();

    assert!(!offload(&mut server).await, "plaintext was still unread");
    assert!(server.kernel_tx_socket().is_none());
    let mut rest = [0; 2];
    server.read_exact(&mut rest).await.unwrap();
    assert_eq!(&rest, b"bc");
    assert!(offload(&mut server).await, "everything was read");
}

#[tokio::test]
async fn a_connection_that_cannot_be_converted_serves_every_byte_from_user_space() {
    let kernel = kernel();
    let case = tls13_aes();
    let refusals = [
        ("not offered to the kernel", true, None),
        ("secret extraction disabled", false, kernel),
    ];
    for (name, extraction, offered) in refusals {
        let (mut server, mut client) = serve(&case, extraction, offered).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(!offload(&mut server).await, "{name}");
        assert!(server.kernel_tx_socket().is_none(), "{name}");
        move_large_bodies(&mut server, &mut client).await;
        AsyncWriteExt::shutdown(&mut server).await.unwrap();
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
    }
}

#[tokio::test]
async fn offloading_twice_keeps_the_kernel_connection() {
    let Some(kernel) = kernel() else { return };
    let (mut server, mut client) = serve(&tls13_aes(), true, Some(kernel)).await;
    talk_in_user_space(&mut server, &mut client).await;
    assert!(offload(&mut server).await);
    assert!(offload(&mut server).await);
    move_large_bodies(&mut server, &mut client).await;
}
