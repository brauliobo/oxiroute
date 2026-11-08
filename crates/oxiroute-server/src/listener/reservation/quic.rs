use std::{
    io,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use quinn::{Endpoint, EndpointConfig, ServerConfig};
use tokio::{runtime::Builder, sync::oneshot};

/// The only QUIC endpoint that reads a UDP reservation.
///
/// Generations that reuse the reservation accept from this endpoint in turn. A second endpoint on
/// the same socket would take a share of the datagrams of every connection, including those of
/// the other generation, and drop them.
pub(super) struct QuicHost {
    endpoint: Endpoint,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl QuicHost {
    pub(super) fn start(socket: std::net::UdpSocket, config: ServerConfig) -> io::Result<Self> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let (stop, stopped) = oneshot::channel::<()>();
        let (ready, endpoint) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("oxiroute-quic-endpoint".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let endpoint = Endpoint::new(
                        EndpointConfig::default(),
                        Some(config),
                        socket,
                        Arc::new(quinn::TokioRuntime),
                    );
                    let started = endpoint.is_ok();
                    ready.send(endpoint).expect("QUIC endpoint receiver");
                    if started {
                        drop(stopped.await);
                    }
                });
            })?;
        Ok(Self {
            endpoint: endpoint.recv().expect("QUIC endpoint thread")?,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub(super) fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
}

impl Drop for QuicHost {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}
