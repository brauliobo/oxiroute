//! The keys of a converted TLS 1.3 connection roll: when the client sends a `KeyUpdate`, and when the sending key has
//! protected as many records as it may. The client reads through a tap that sees the records as they cross the wire.

use std::{
    io::{self, Write},
    net::Ipv4Addr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
};

use oxiroute_tls_rustls::KernelTls;
use pingora::{
    listeners::StreamTlsAcceptor,
    protocols::{
        Stream,
        l4::{sendfile::send_file_to_stream, stream::Stream as L4Stream},
    },
};
use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

use super::{
    TIMEOUT, acceptor, cases, common::NAME, kernel, offload, pair_of, pattern, push,
    talk_in_user_space,
};

const RECORD: usize = 16 * 1024;
/// The wire length of a `KeyUpdate` record in TLS 1.3: the 5 byte message, its content type and the 16 byte tag.
const KEY_UPDATE_RECORD: usize = 22;

struct Tap {
    socket: TcpStream,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for Tap {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        ready!(Pin::new(&mut self.socket).poll_read(context, buffer))?;
        let arrived = &buffer.filled()[before..];
        self.seen.lock().unwrap().extend_from_slice(arrived);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Tap {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(context)
    }
}

struct Wire(Arc<Mutex<Vec<u8>>>);

impl Wire {
    /// How many `KeyUpdate` records the server has sent: application records of one byte do not occur in these
    /// tests, and nothing else the server sends is that long.
    fn key_updates(&self) -> usize {
        let seen = self.0.lock().unwrap();
        let (mut at, mut count) = (0, 0);
        while at + 5 <= seen.len() {
            let length = usize::from(u16::from_be_bytes([seen[at + 3], seen[at + 4]]));
            count += usize::from(seen[at] == 23 && length == KEY_UPDATE_RECORD);
            at += 5 + length;
        }
        count
    }
}

async fn serve_tapped(case: &super::Case, kernel: KernelTls) -> (Stream, TlsStream<Tap>, Wire) {
    let pair = pair_of(case, true);
    let acceptor = acceptor(&pair, Some(kernel));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        acceptor.accept(L4Stream::from(socket)).await
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tap = Tap {
        socket: TcpStream::connect(address).await.unwrap(),
        seen: Arc::clone(&seen),
    };
    let client = TlsConnector::from(Arc::new(pair.client))
        .connect(ServerName::try_from(NAME).unwrap(), tap)
        .await
        .unwrap();
    (server.await.unwrap().unwrap(), client, Wire(seen))
}

fn tls13_cases() -> impl Iterator<Item = super::Case> {
    cases()
        .into_iter()
        .filter(|case| case.version.version == rustls::ProtocolVersion::TLSv1_3)
}

#[tokio::test]
async fn a_key_update_of_the_client_after_the_conversion_is_answered_and_followed() {
    let Some(kernel) = kernel() else { return };
    for case in tls13_cases() {
        let (mut server, mut client, wire) = serve_tapped(&case, kernel).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await, "{}", case.name);
        for round in 1..=3 {
            client.get_mut().1.refresh_traffic_keys().unwrap();
            push(&mut client, &mut server, b"after the key update").await;
            push(&mut server, &mut client, b"answered with the new keys").await;
            super::move_large_bodies(&mut server, &mut client).await;
            assert_eq!(wire.key_updates(), round, "{}", case.name);
        }
    }
}

#[tokio::test]
async fn the_sending_key_rolls_before_it_protects_more_records_than_it_may() {
    let Some(kernel) = kernel() else { return };
    let kernel = kernel.rolling_keys_after(4);
    for case in tls13_cases() {
        let (mut server, mut client, wire) = serve_tapped(&case, kernel).await;
        talk_in_user_space(&mut server, &mut client).await;
        assert!(offload(&mut server).await, "{}", case.name);
        let body = pattern(48 * RECORD);
        for chunk in body.chunks(RECORD) {
            push(&mut server, &mut client, chunk).await;
        }
        push(
            &mut client,
            &mut server,
            b"the other direction is untouched",
        )
        .await;
        assert!(
            wire.key_updates() >= 10,
            "{}: {}",
            case.name,
            wire.key_updates()
        );
    }
}

#[tokio::test]
async fn files_sent_with_sendfile_roll_the_sending_key_between_them() {
    let Some(kernel) = kernel() else { return };
    let kernel = kernel.rolling_keys_after(4);
    for case in tls13_cases() {
        let body = pattern(4 * RECORD);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&body).unwrap();
        let (mut server, mut client, wire) = serve_tapped(&case, kernel).await;
        talk_in_user_space(&mut server, &mut client).await;
        for _ in 0..8 {
            let mut received = vec![0; body.len()];
            let (sent, read) = tokio::join!(
                send_file_to_stream(&mut server, &file, 0, body.len()),
                timeout(TIMEOUT, client.read_exact(&mut received))
            );
            assert_eq!(sent.unwrap(), Some(body.len()), "{}", case.name);
            read.unwrap().unwrap();
            assert!(received == body, "{}: the file changed", case.name);
        }
        assert!(
            wire.key_updates() >= 6,
            "{}: {}",
            case.name,
            wire.key_updates()
        );
        push(&mut client, &mut server, b"next request").await;
    }
}

#[tokio::test]
async fn a_key_update_after_a_roll_of_the_sending_key_still_works() {
    let Some(kernel) = kernel() else { return };
    let kernel = kernel.rolling_keys_after(2);
    let case = super::tls13_aes();
    let (mut server, mut client, wire) = serve_tapped(&case, kernel).await;
    talk_in_user_space(&mut server, &mut client).await;
    assert!(offload(&mut server).await);
    super::move_large_bodies(&mut server, &mut client).await;
    let rolled = wire.key_updates();
    assert!(rolled > 0);
    client.get_mut().1.refresh_traffic_keys().unwrap();
    push(&mut client, &mut server, b"after the key update").await;
    push(&mut server, &mut client, b"answered with the new keys").await;
    super::move_large_bodies(&mut server, &mut client).await;
    assert!(wire.key_updates() > rolled);
}
