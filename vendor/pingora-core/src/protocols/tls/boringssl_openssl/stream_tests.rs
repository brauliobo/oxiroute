//! Record coalescing of accepted TLS streams, over an in-memory transport that records its writes.

use super::SslStream;
use crate::tls::ssl::{Ssl, SslAcceptor, SslMethod, SslVerifyMode};
use crate::tls::{pkey::PKey, x509::X509};
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{
    duplex, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf,
};

/// A transport that remembers the size of every write handed to it and counts its reads.
struct Recorder {
    inner: DuplexStream,
    writes: Arc<Mutex<Vec<usize>>>,
    reads: Arc<AtomicUsize>,
    /// What the read probe answers.
    readable: Arc<AtomicBool>,
}

impl AsyncRead for Recorder {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Recorder {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = futures::ready!(Pin::new(&mut self.inner).poll_write(cx, buf))?;
        self.writes.lock().unwrap().push(written);
        Poll::Ready(Ok(written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Both ends of a handshaken connection and what the server's transport saw.
struct Harness {
    server: SslStream<Recorder>,
    client: SslStream<DuplexStream>,
    writes: Arc<Mutex<Vec<usize>>>,
    reads: Arc<AtomicUsize>,
    readable: Arc<AtomicBool>,
}

fn probe(transport: &mut Recorder, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    if transport.readable.load(Ordering::Relaxed) {
        Poll::Ready(Ok(()))
    } else {
        Poll::Pending
    }
}

/// Handshakes a server stream (coalescing or not) and a client.
async fn connected(coalesce: bool) -> Harness {
    let (cert, key) = crate::test_tls::generate_pem();
    let mut acceptor = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    acceptor
        .set_certificate(&X509::from_pem(&cert).unwrap())
        .unwrap();
    acceptor
        .set_private_key(&PKey::private_key_from_pem(&key).unwrap())
        .unwrap();
    let acceptor = acceptor.build();
    let (client_io, server_io) = duplex(1 << 20);
    let writes = Arc::new(Mutex::new(Vec::new()));
    let reads = Arc::new(AtomicUsize::new(0));
    let readable = Arc::new(AtomicBool::new(true));
    let server_io = Recorder {
        inner: server_io,
        writes: Arc::clone(&writes),
        reads: Arc::clone(&reads),
        readable: Arc::clone(&readable),
    };
    let mut server = SslStream::new(
        crate::tls::ext::ssl_from_acceptor(&acceptor).unwrap(),
        server_io,
    )
    .unwrap();
    if coalesce {
        server.coalesce_writes();
        server.probe_reads(probe);
    }
    let mut context = crate::tls::ssl::SslContext::builder(SslMethod::tls()).unwrap();
    context.set_verify(SslVerifyMode::NONE);
    let mut client = SslStream::new(Ssl::new(&context.build()).unwrap(), client_io).unwrap();
    let (accepted, connected) = tokio::join!(server.accept(), client.connect());
    accepted.unwrap();
    connected.unwrap();
    writes.lock().unwrap().clear();
    Harness {
        server,
        client,
        writes,
        reads,
        readable,
    }
}

async fn read_exact(client: &mut SslStream<DuplexStream>, len: usize) -> Vec<u8> {
    let mut received = vec![0; len];
    client.read_exact(&mut received).await.unwrap();
    received
}

/// One TLS record around `len` bytes of plaintext: 5 byte header, content type, 16 byte tag.
const fn record(len: usize) -> usize {
    5 + len + 1 + 16
}

#[tokio::test]
async fn writes_between_flushes_share_one_record() {
    let Harness {
        mut server,
        mut client,
        writes,
        ..
    } = connected(true).await;
    server.write_all(&[b'h'; 230]).await.unwrap();
    server.write_all(&[b'b'; 1024]).await.unwrap();
    assert!(
        writes.lock().unwrap().is_empty(),
        "nothing leaves before the flush"
    );
    server.flush().await.unwrap();
    assert_eq!(*writes.lock().unwrap(), [record(1254)]);
    let received = read_exact(&mut client, 1254).await;
    assert_eq!(&received[..230], &[b'h'; 230][..]);
    assert_eq!(&received[230..], &[b'b'; 1024][..]);
}

#[tokio::test]
async fn vectored_write_shares_one_record() {
    let Harness {
        mut server,
        mut client,
        writes,
        ..
    } = connected(true).await;
    let (head, body) = ([b'h'; 40], vec![b'b'; 3000]);
    let written = server
        .write_vectored(&[IoSlice::new(&head), IoSlice::new(&body)])
        .await
        .unwrap();
    assert_eq!(written, 3040);
    server.flush().await.unwrap();
    assert_eq!(*writes.lock().unwrap(), [record(3040)]);
    assert_eq!(read_exact(&mut client, 3040).await[40..], body[..]);
}

#[tokio::test]
async fn large_write_goes_out_after_the_pending_bytes_in_order() {
    let Harness {
        mut server,
        mut client,
        writes,
        ..
    } = connected(true).await;
    server.write_all(&[b'a'; 100]).await.unwrap();
    server.write_all(&[b'z'; 5000]).await.unwrap();
    assert_eq!(*writes.lock().unwrap(), [record(100), record(5000)]);
    server.flush().await.unwrap();
    let received = read_exact(&mut client, 5100).await;
    assert!(received[..100].iter().all(|byte| *byte == b'a'));
    assert!(received[100..].iter().all(|byte| *byte == b'z'));
}

#[tokio::test]
async fn pending_bytes_leave_before_a_shutdown() {
    let Harness {
        mut server,
        mut client,
        ..
    } = connected(true).await;
    server.write_all(b"last words").await.unwrap();
    server.shutdown().await.unwrap();
    let mut received = Vec::new();
    client.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, b"last words");
}

#[tokio::test]
async fn writes_are_not_held_back_unless_asked() {
    let Harness {
        mut server,
        client: _client,
        writes,
        ..
    } = connected(false).await;
    server.write_all(&[b'h'; 230]).await.unwrap();
    server.write_all(&[b'b'; 1024]).await.unwrap();
    assert_eq!(*writes.lock().unwrap(), [record(230), record(1024)]);
}

#[tokio::test]
async fn a_read_that_ran_dry_waits_for_the_transport_instead_of_running_the_record_layer() {
    let Harness {
        mut server,
        mut client,
        reads,
        readable,
        ..
    } = connected(true).await;
    let mut buf = [0; 64];
    let mut poll_server = |server: &mut SslStream<Recorder>| {
        let mut read_buf = ReadBuf::new(&mut buf);
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        Pin::new(server)
            .poll_read(&mut cx, &mut read_buf)
            .is_pending()
    };

    readable.store(false, Ordering::Relaxed);
    assert!(poll_server(&mut server));
    let after_first = reads.load(Ordering::Relaxed);
    assert!(
        after_first > 0,
        "the first read finds out the transport is dry"
    );
    for _ in 0..3 {
        assert!(poll_server(&mut server));
    }
    assert_eq!(
        reads.load(Ordering::Relaxed),
        after_first,
        "dry reads are skipped"
    );

    client.write_all(b"ping").await.unwrap();
    client.flush().await.unwrap();
    readable.store(true, Ordering::Relaxed);
    let mut received = [0; 4];
    server.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"ping");
}

#[tokio::test]
async fn vectored_bytes_are_packed_into_full_records() {
    use bytes::Buf;
    let Harness {
        mut server,
        mut client,
        writes,
        ..
    } = connected(true).await;
    let (frame_header, payload) = ([b'h'; 9], vec![b'p'; 16384]);
    // Two HTTP/2 DATA frames: 9 header bytes and 16384 payload bytes each.
    let mut frames = Buf::chain(
        Buf::chain(
            Buf::chain(&frame_header[..], &payload[..]),
            &frame_header[..],
        ),
        &payload[..],
    );
    server.write_all_buf(&mut frames).await.unwrap();
    server.flush().await.unwrap();
    assert_eq!(
        *writes.lock().unwrap(),
        [record(16384), record(16384), record(18)],
        "full records, not a short record per frame header"
    );
    assert_eq!(read_exact(&mut client, 32786).await.len(), 32786);
}
