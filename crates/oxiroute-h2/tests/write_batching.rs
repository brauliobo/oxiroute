//! The connection writes what the tasks that are ready queued in one go: a response queued while the driver is
//! parked is not written before the scheduler had a turn for the other ready tasks.
use std::{
    future::Future,
    io,
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use http::{HeaderMap, Request, StatusCode};
use oxiroute_h2::server::{Config, Server};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf, duplex};

/// A transport that counts the writes the connection makes.
struct Counted {
    io: DuplexStream,
    writes: Arc<AtomicUsize>,
}

impl AsyncRead for Counted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Counted {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_queued_response_is_written_after_one_turn_of_the_scheduler() {
    let (client_io, server_io) = duplex(1 << 20);
    let client = tokio::spawn(async move {
        let (sender, connection) = h2::client::handshake(client_io).await.unwrap();
        tokio::spawn(connection);
        let request = Request::builder()
            .uri("https://example.com/")
            .body(())
            .unwrap();
        let (response, _) = sender
            .ready()
            .await
            .unwrap()
            .send_request(request, true)
            .unwrap();
        response.await.unwrap().status()
    });
    let writes = Arc::new(AtomicUsize::new(0));
    let io = Counted {
        io: server_io,
        writes: writes.clone(),
    };
    let mut server = Server::handshake(io, Config::default()).await.unwrap();
    let (_view, mut stream) = server.accept_view().await.unwrap().unwrap();
    let before = writes.load(Ordering::SeqCst);

    stream
        .send_response(StatusCode::OK, HeaderMap::new(), true)
        .unwrap();
    let counter = Arc::new(Wakes::default());
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);
    let mut accept = pin!(server.accept_view());
    assert!(accept.as_mut().poll(&mut cx).is_pending());
    assert_eq!(
        writes.load(Ordering::SeqCst),
        before,
        "nothing is written before the other ready tasks had their turn"
    );
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "the driver asked for another turn"
    );

    assert!(accept.as_mut().poll(&mut cx).is_pending());
    assert!(
        writes.load(Ordering::SeqCst) > before,
        "the response is written on the next turn"
    );
    assert_eq!(client.await.unwrap(), StatusCode::OK);
}
