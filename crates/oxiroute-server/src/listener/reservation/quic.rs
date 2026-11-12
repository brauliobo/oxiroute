use std::{
    io,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use quinn::{Endpoint, EndpointConfig, ServerConfig};
use tokio::{runtime::Builder, sync::oneshot};

/// The only QUIC endpoints that read a UDP reservation: one per socket, each on a thread of its own.
///
/// Generations that reuse the reservation accept from these endpoints in turn. A second endpoint on
/// the same socket would take a share of the datagrams of every connection, including those of
/// the other generation, and drop them. The sockets of a host are the reservation's own and, for a
/// host of several endpoints, siblings that joined its `SO_REUSEPORT` group: the kernel gives every
/// peer address to one of them, and a connection never migrates, so it stays with its endpoint.
pub(super) struct QuicHost {
    endpoints: Vec<Endpoint>,
    members: Vec<Member>,
}

struct Member {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl QuicHost {
    pub(super) fn start(
        sockets: Vec<std::net::UdpSocket>,
        config: &ServerConfig,
    ) -> io::Result<Self> {
        let mut host = Self {
            endpoints: Vec::with_capacity(sockets.len()),
            members: Vec::with_capacity(sockets.len()),
        };
        for socket in sockets {
            let (endpoint, member) = Member::start(socket, config.clone())?;
            host.endpoints.push(endpoint);
            host.members.push(member);
        }
        Ok(host)
    }

    pub(super) fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoints.clone()
    }
}

impl Member {
    fn start(socket: std::net::UdpSocket, config: ServerConfig) -> io::Result<(Endpoint, Self)> {
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
        let endpoint = endpoint.recv().expect("QUIC endpoint thread")?;
        Ok((
            endpoint,
            Self {
                stop: Some(stop),
                thread: Some(thread),
            },
        ))
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}
