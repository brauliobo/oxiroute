//! A request that ends in a tunnel: the connection runs the tunnel after the request future is gone, then closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Notify};
use tokio::time::timeout;

use super::{ErasedFuture, HttpServerApp, ReusedHttpStream, ServerApp};
use crate::protocols::http::ServerSession;
use crate::protocols::l4::stream::Stream as L4Stream;
use crate::server::ShutdownWatch;

struct Upgrading {
    started: Arc<Notify>,
    release: Arc<Notify>,
    finished: Arc<AtomicBool>,
}

#[async_trait]
impl HttpServerApp for Upgrading {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        _shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        session.read_request().await.unwrap();
        let (started, release, finished) = (
            self.started.clone(),
            self.release.clone(),
            self.finished.clone(),
        );
        Some(ReusedHttpStream::tunnel(ErasedFuture::new(
            move || async move {
                started.notify_one();
                release.notified().await;
                finished.store(true, Ordering::SeqCst);
            },
        )))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_runs_the_tunnel_of_its_request_and_then_ends() {
    let (started, release, finished) = (
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(AtomicBool::new(false)),
    );
    let app = Arc::new(Upgrading {
        started: started.clone(),
        release: release.clone(),
        finished: finished.clone(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let (_shutdown, watch) = watch::channel(false);
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n")
        .await
        .unwrap();

    let mut server = tokio::spawn(async move {
        app.process_new(Box::new(L4Stream::from(accepted)), &watch)
            .await
    });
    timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the tunnel was never started");
    assert!(
        timeout(Duration::from_millis(100), &mut server)
            .await
            .is_err(),
        "the connection ended while its tunnel was running"
    );
    release.notify_one();
    let reused = timeout(Duration::from_secs(5), server)
        .await
        .expect("the connection did not end with its tunnel")
        .unwrap();
    assert!(
        reused.is_none(),
        "an upgraded connection is not served again"
    );
    assert!(finished.load(Ordering::SeqCst));
}
