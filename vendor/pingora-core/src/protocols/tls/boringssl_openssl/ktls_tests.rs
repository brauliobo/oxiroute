//! Kernel TLS transmit offload of an accepted connection, against blocking OpenSSL clients on loopback.
//!
//! The tests need the kernel's `tls` module (`sudo modprobe tls`); without it each test says why on stderr and passes
//! without running.

use super::server::{handshake, offer_kernel_tx};
use super::SslStream;
use crate::protocols::l4::ktls::probe;
use crate::protocols::l4::sendfile::send_file;
use crate::protocols::l4::stream::Stream;
use crate::protocols::Ssl;
use crate::tls::ssl::{
    SslAcceptor, SslConnector, SslConnectorBuilder, SslFiletype, SslMethod, SslVerifyMode,
    SslVersion,
};
use std::fs::File;
use std::future::poll_fn;
use std::io::{Read, Write};
use std::net::TcpStream as StdTcpStream;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Client = crate::tls::ssl::SslStream<StdTcpStream>;

fn kernel_offers_ktls() -> bool {
    match probe() {
        Ok(()) => true,
        Err(unavailable) => {
            eprintln!("skipped: kernel TLS is not available on this host: {unavailable}");
            false
        }
    }
}

/// A counter of the kernel's TLS statistics for this network namespace (`/proc/net/tls_stat`).
fn tls_stat(name: &str) -> u64 {
    std::fs::read_to_string("/proc/net/tls_stat")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
        .unwrap_or_else(|| panic!("no {name} in /proc/net/tls_stat"))
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| ((i * 7 + i / 251) % 251) as u8)
        .collect()
}

fn file_with(bytes: &[u8]) -> File {
    let path = std::env::temp_dir().join(format!("ktls-test-{}", rand::random::<u64>()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file.write_all(bytes).unwrap();
    file
}

fn acceptor(cert: &crate::test_tls::TestCert) -> SslAcceptor {
    let mut builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    builder.set_certificate_chain_file(&cert.cert_path).unwrap();
    builder
        .set_private_key_file(&cert.key_path, SslFiletype::PEM)
        .unwrap();
    builder.build()
}

/// A handshaken server stream, and the thread of the blocking client that runs `script` on its end.
async fn connected(
    offer: bool,
    configure: impl FnOnce(&mut SslConnectorBuilder) + Send + 'static,
    script: impl FnOnce(Client) + Send + 'static,
) -> (SslStream<Stream>, std::thread::JoinHandle<()>) {
    let cert = crate::test_tls::TestCert::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = std::thread::spawn(move || {
        let mut builder = SslConnector::builder(SslMethod::tls()).unwrap();
        builder.set_verify(SslVerifyMode::NONE);
        configure(&mut builder);
        let tcp = StdTcpStream::connect(address).unwrap();
        script(builder.build().connect("localhost", tcp).unwrap());
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut server = handshake(&acceptor(&cert), Stream::from(tcp))
        .await
        .unwrap();
    if offer {
        offer_kernel_tx(&mut server);
    }
    (server, client)
}

async fn convert(server: &mut SslStream<Stream>) -> bool {
    poll_fn(|cx| server.poll_kernel_tx(cx)).await
}

/// What the HTTP session does for a file: let the stream settle its keys, then `sendfile`.
async fn send(server: &mut SslStream<Stream>, file: &File, offset: u64, length: usize) {
    assert!(convert(server).await);
    let socket = server
        .kernel_tx_socket()
        .expect("the kernel encrypts this connection");
    assert_eq!(
        send_file(socket, file, offset, length).await.unwrap(),
        length
    );
}

fn read_exact(client: &mut Client, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    client.read_exact(&mut bytes).unwrap();
    bytes
}

fn request_key_update(client: &mut Client) {
    use foreign_types::ForeignTypeRef;
    unsafe extern "C" {
        fn SSL_key_update(
            ssl: *mut crate::tls::ssl_sys::SSL,
            update_type: std::os::raw::c_int,
        ) -> i32;
    }
    // SAFETY: the pointer is the live SSL of `client`; 1 is SSL_KEY_UPDATE_REQUESTED.
    assert_eq!(unsafe { SSL_key_update(client.ssl().as_ptr(), 1) }, 1);
}

#[tokio::test]
async fn a_converted_connection_sends_records_and_files_the_client_decrypts() {
    if !kernel_offers_ktls() {
        return;
    }
    let data = pattern(3_000_000);
    let expected = data.clone();
    let before = tls_stat("TlsTxSw");
    let (mut server, client) = connected(
        true,
        |_| {},
        move |mut client| {
            assert_eq!(read_exact(&mut client, 5), b"hello");
            assert_eq!(read_exact(&mut client, 3_000_000), expected);
            assert_eq!(read_exact(&mut client, 4), b"tail");
        },
    )
    .await;
    assert!(convert(&mut server).await);
    assert!(server.kernel_tx_socket().is_some());
    assert!(
        tls_stat("TlsTxSw") > before,
        "the kernel does not count a software TLS socket"
    );
    server.write_all(b"hello").await.unwrap();
    server.flush().await.unwrap();
    send(&mut server, &file_with(&data), 0, data.len()).await;
    server.write_all(b"tail").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn bytes_waiting_in_the_coalescing_buffer_leave_before_the_keys_change() {
    if !kernel_offers_ktls() {
        return;
    }
    let (mut server, client) = connected(
        true,
        |_| {},
        |mut client| {
            assert_eq!(read_exact(&mut client, 10), b"head-body!");
        },
    )
    .await;
    server.write_all(b"head-").await.unwrap();
    assert!(convert(&mut server).await);
    server.write_all(b"body!").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn a_key_update_requested_by_the_client_is_answered_with_new_kernel_keys() {
    if !kernel_offers_ktls() {
        return;
    }
    let data = pattern(2_000_000);
    let expected = data.clone();
    let rekeys = tls_stat("TlsTxRekeyOk");
    let (mut server, client) = connected(
        true,
        |_| {},
        move |mut client| {
            assert_eq!(read_exact(&mut client, 1_000_000), expected[..1_000_000]);
            request_key_update(&mut client);
            client.write_all(b"k").unwrap();
            assert_eq!(read_exact(&mut client, 1_000_000), expected[1_000_000..]);
            client.write_all(b"again").unwrap();
            assert_eq!(read_exact(&mut client, 2), b"ok");
        },
    )
    .await;
    assert!(convert(&mut server).await);
    let file = file_with(&data);
    send(&mut server, &file, 0, 1_000_000).await;
    let mut byte = [0_u8; 1];
    server.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"k");
    send(&mut server, &file, 1_000_000, 1_000_000).await;
    assert!(
        tls_stat("TlsTxRekeyOk") > rekeys,
        "the kernel did not rekey the transmit direction"
    );
    let mut again = [0_u8; 5];
    server.read_exact(&mut again).await.unwrap();
    assert_eq!(&again, b"again");
    server.write_all(b"ok").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn tls_1_2_connections_stay_in_user_space() {
    let (mut server, client) = connected(
        true,
        |builder| {
            builder
                .set_max_proto_version(Some(SslVersion::TLS1_2))
                .unwrap()
        },
        |mut client| assert_eq!(read_exact(&mut client, 2), b"ok"),
    )
    .await;
    assert!(!convert(&mut server).await);
    assert!(server.kernel_tx_socket().is_none());
    server.write_all(b"ok").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn a_cipher_the_offload_does_not_cover_stays_in_user_space() {
    let (mut server, client) = connected(
        true,
        |builder| {
            builder
                .set_ciphersuites("TLS_CHACHA20_POLY1305_SHA256")
                .unwrap();
        },
        |mut client| assert_eq!(read_exact(&mut client, 2), b"ok"),
    )
    .await;
    assert!(!convert(&mut server).await);
    server.write_all(b"ok").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn a_connection_that_was_not_offered_the_offload_is_never_converted() {
    let (mut server, client) = connected(
        false,
        |_| {},
        |mut client| {
            assert_eq!(read_exact(&mut client, 2), b"ok");
        },
    )
    .await;
    assert!(!convert(&mut server).await);
    assert!(server.kernel_tx_socket().is_none());
    server.write_all(b"ok").await.unwrap();
    server.flush().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn the_server_closes_with_a_close_notify_the_client_recognises() {
    if !kernel_offers_ktls() {
        return;
    }
    let (mut server, client) = connected(
        true,
        |_| {},
        |mut client| {
            assert_eq!(read_exact(&mut client, 3), b"bye");
            let mut rest = Vec::new();
            assert_eq!(
                client.read_to_end(&mut rest).unwrap(),
                0,
                "a clean end, not a truncation"
            );
            client.shutdown().unwrap();
        },
    )
    .await;
    assert!(convert(&mut server).await);
    server.write_all(b"bye").await.unwrap();
    server.shutdown().await.unwrap();
    client.join().unwrap();
}

#[tokio::test]
async fn a_client_that_closed_its_side_still_receives_the_rest_of_the_file() {
    if !kernel_offers_ktls() {
        return;
    }
    let data = pattern(1_500_000);
    let expected = data.clone();
    let (mut server, client) = connected(
        true,
        |_| {},
        move |mut client| {
            client.write_all(b"request").unwrap();
            client.shutdown().unwrap();
            assert_eq!(read_exact(&mut client, 1_500_000), expected);
        },
    )
    .await;
    assert!(convert(&mut server).await);
    let mut request = [0_u8; 7];
    server.read_exact(&mut request).await.unwrap();
    let mut end = Vec::new();
    assert_eq!(server.read_to_end(&mut end).await.unwrap(), 0);
    send(&mut server, &file_with(&data), 0, data.len()).await;
    client.join().unwrap();
}

#[tokio::test]
async fn a_client_that_reads_late_gets_every_byte_of_writes_and_files() {
    if !kernel_offers_ktls() {
        return;
    }
    let data = pattern(6_000_000);
    let expected = data.clone();
    let (mut server, client) = connected(
        true,
        |_| {},
        move |mut client| {
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(read_exact(&mut client, 6_000_000), expected);
            assert_eq!(read_exact(&mut client, 6_000_000), expected);
        },
    )
    .await;
    assert!(convert(&mut server).await);
    server.write_all(&data).await.unwrap();
    server.flush().await.unwrap();
    send(&mut server, &file_with(&data), 0, data.len()).await;
    client.join().unwrap();
}
