#[path = "support/ports.rs"]
mod ports_support;
use std::{
    io::Read as _,
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use async_trait::async_trait;
use pingora::{
    apps::{AcceptGate, ConnectionAdmission, ServerApp},
    protocols::Stream,
    server::ShutdownWatch,
    services::{Service as _, listening::Service},
};
use tokio::{sync::watch, time::sleep};

/// Marks its slot as free when dropped.
struct Slot(Arc<AtomicBool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct AdmissionApp {
    released: Arc<AtomicBool>,
    gate: Option<AcceptGate>,
}

#[async_trait]
impl ServerApp for AdmissionApp {
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.gate.clone()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        Some(Box::new(Slot(Arc::clone(&self.released))))
    }

    // `async_trait` boxes an `async fn`, which is what `ServerApp::process_new` avoids.
    #[allow(clippy::manual_async_fn)]
    fn process_new(
        self: &Arc<Self>,
        stream: Stream,
        _shutdown: &ShutdownWatch,
    ) -> impl std::future::Future<Output = Option<Stream>> + Send {
        async move {
            // Like an app that awaits cleanup tasks after its connection closed: the client sees the
            // close here, long before the service task ends.
            drop(stream);
            sleep(Duration::from_millis(200)).await;
            None
        }
    }
}

/// A client that reconnects the moment it sees EOF must find its admission slot already free.
async fn assert_slot_released_before_client_sees_close(gate: Option<AcceptGate>) {
    let released = Arc::new(AtomicBool::new(false));
    let address = free_address();
    let app = AdmissionApp {
        released: Arc::clone(&released),
        gate,
    };
    let mut service = Service::new("admission release".into(), app);
    service.add_tcp(&address.to_string());
    let (shutdown, shutdown_watch) = watch::channel(false);
    let client = thread::spawn(move || {
        let mut stream = loop {
            match TcpStream::connect(address) {
                Ok(stream) => break stream,
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        };
        stream
            .read_to_end(&mut Vec::new())
            .expect("read until close");
        let released_at_eof = released.load(Ordering::SeqCst);
        shutdown.send(true).expect("stop service");
        released_at_eof
    });

    service.start_service(None, shutdown_watch, 1).await;

    assert!(
        client.join().expect("client thread"),
        "the socket closed while its slot was held"
    );
}

fn free_address() -> SocketAddr {
    ports_support::unused_address()
}

#[tokio::test]
async fn admission_is_released_before_the_client_observes_close() {
    assert_slot_released_before_client_sees_close(None).await;
}

#[tokio::test]
async fn gated_admission_is_released_before_the_client_observes_close() {
    let gate = AcceptGate::closed();
    gate.enable();
    assert_slot_released_before_client_sees_close(Some(gate)).await;
}
