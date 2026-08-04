use std::{
    error::Error,
    io,
    sync::Arc,
    thread::{self, JoinHandle},
};

use h3::server::RequestResolver;
use log::{error, warn};
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, EndpointConfig, ServerConfig, TransportConfig, VarInt};
use rustls::server::{ClientHello, ResolvesServerCert};
use tokio::{
    runtime::Builder,
    sync::{watch, Semaphore},
};

use crate::{
    ForwardHttp1ServicePlan, ListenerMetrics, ListenerReservation, RuntimeGeneration,
    RuntimeReferenceKind, TlsProfilePlan,
};

const H3_HANDSHAKE_LIMIT: usize = 64;
const H3_BIDI_STREAM_LIMIT: u32 = 128;
const H3_UNI_STREAM_LIMIT: u32 = 16;
const H3_STREAM_RECEIVE_WINDOW: u32 = 1024 * 1024;
const H3_CONNECTION_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
const H3_INCOMING_BUFFER: u64 = 1024 * 1024;
const H3_TOTAL_INCOMING_BUFFER: u64 = 16 * 1024 * 1024;
const H3_CLOSE_CODE: VarInt = VarInt::from_u32(0);

pub struct Http3Runtime {
    thread: Option<JoinHandle<()>>,
}

impl Http3Runtime {
    /// Starts one active forward HTTP/3 listener on a previously reserved UDP socket.
    ///
    /// # Errors
    ///
    /// Returns an error unless the QUIC endpoint, TLS configuration, and H3 accept loop all reach
    /// their startup boundary.
    pub fn start_forward(
        listener_name: String,
        reservation: ListenerReservation,
        service: Arc<ForwardHttp1ServicePlan>,
        tls: Arc<TlsProfilePlan>,
        generation: Arc<RuntimeGeneration>,
        metrics: ListenerMetrics,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, Box<dyn Error>> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let startup_listener_name = listener_name.clone();
        let thread = thread::Builder::new()
            .name(format!("oxiroute-h3-{listener_name}"))
            .spawn(move || {
                let result = run_forward(
                    &listener_name,
                    reservation,
                    service,
                    tls,
                    generation.clone(),
                    metrics,
                    shutdown,
                    ready_tx,
                );
                if let Err(error) = result {
                    generation.mark_runtime_failed();
                    error!("HTTP/3 listener `{listener_name}` failed: {error}");
                }
            })?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(io::Error::other(error).into())
            }
            Err(error) => {
                let _ = thread.join();
                Err(io::Error::new(
                    if error == std::sync::mpsc::RecvTimeoutError::Timeout {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    },
                    format!(
                        "HTTP/3 listener `{startup_listener_name}` did not become ready: {error}"
                    ),
                )
                .into())
            }
        }
    }

    /// Waits for the listener and all active H3 connections to stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener thread panicked.
    ///
    /// # Panics
    ///
    /// Panics if the listener thread handle was already consumed, which violates the runtime
    /// ownership invariant.
    pub fn join(mut self) -> io::Result<()> {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return self
                .thread
                .take()
                .expect("finished HTTP/3 listener thread")
                .join()
                .map_err(|_| io::Error::other("HTTP/3 listener thread panicked"));
        }
        self.thread
            .take()
            .expect("HTTP/3 listener thread exists")
            .join()
            .map_err(|_| io::Error::other("HTTP/3 listener thread panicked"))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_forward(
    listener_name: &str,
    reservation: ListenerReservation,
    service: Arc<ForwardHttp1ServicePlan>,
    tls: Arc<TlsProfilePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    shutdown: watch::Receiver<bool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        let socket = reservation.duplicate_udp_socket()?;
        let server_config = server_config(&tls)?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        metrics.mark_listening();
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::other("HTTP/3 startup receiver was dropped"))?;
        let listener_metrics = metrics.clone();
        let orderly = serve_endpoint(
            listener_name,
            endpoint,
            service,
            generation,
            metrics,
            shutdown,
        )
        .await;
        if orderly {
            listener_metrics.mark_stopped();
        } else {
            listener_metrics.mark_failed();
            return Err(io::Error::other("HTTP/3 endpoint terminated unexpectedly").into());
        }
        Ok::<(), Box<dyn Error>>(())
    })
}

fn server_config(tls: &Arc<TlsProfilePlan>) -> io::Result<ServerConfig> {
    let resolver = Arc::new(QuicCertificateResolver {
        profile: Arc::clone(tls),
    });
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).map_err(io::Error::other)?,
    ));
    config
        .max_incoming(H3_HANDSHAKE_LIMIT)
        .incoming_buffer_size(H3_INCOMING_BUFFER)
        .incoming_buffer_size_total(H3_TOTAL_INCOMING_BUFFER)
        .migration(false);
    let mut transport = TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(VarInt::from_u32(H3_BIDI_STREAM_LIMIT))
        .max_concurrent_uni_streams(VarInt::from_u32(H3_UNI_STREAM_LIMIT))
        .stream_receive_window(VarInt::from_u32(H3_STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(H3_CONNECTION_RECEIVE_WINDOW))
        .allow_spin(false)
        .datagram_receive_buffer_size(None)
        .datagram_send_buffer_size(0);
    config.transport_config(Arc::new(transport));
    Ok(config)
}

async fn serve_endpoint(
    listener_name: &str,
    endpoint: Endpoint,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
) -> bool {
    let handshakes = Arc::new(Semaphore::new(H3_HANDSHAKE_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    let orderly = loop {
        tokio::select! {
            _ = shutdown.changed() => break true,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break false };
                let Ok(handshake) = Arc::clone(&handshakes).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let service = Arc::clone(&service);
                let generation = Arc::clone(&generation);
                let metrics = metrics.clone();
                let shutdown = shutdown.clone();
                connections.spawn(async move {
                    run_connection(incoming, service, generation, metrics, shutdown, handshake).await;
                });
            }
        }
    };
    endpoint.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            warn!("HTTP/3 listener `{listener_name}` connection task failed: {error}");
        }
    }
    orderly
}

async fn run_connection(
    incoming: quinn::Incoming,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
    _handshake: tokio::sync::OwnedSemaphorePermit,
) {
    let connection = match incoming.await {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 QUIC handshake failed: {error}");
            return;
        }
    };
    let client_addr = Some(connection.remote_address());
    let Some(service_connection) = service.begin_connection() else {
        connection.close(H3_CLOSE_CODE, b"service connection limit");
        return;
    };
    let Some(generation_reference) = generation.begin_reference(RuntimeReferenceKind::ForwardHttp3)
    else {
        connection.close(H3_CLOSE_CODE, b"generation draining");
        return;
    };
    let listener_connection = match metrics.begin_connection() {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 connection admission failed: {error}");
            connection.close(H3_CLOSE_CODE, b"listener connection limit");
            return;
        }
    };
    let mut builder = h3::server::builder();
    builder
        .max_field_section_size(u64::try_from(service.max_header_bytes()).unwrap_or(u64::MAX))
        .send_grease(false);
    let mut h3: h3::server::Connection<_, bytes::Bytes> = match builder
        .build(h3_quinn::Connection::new(connection.clone()))
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            warn!("HTTP/3 connection setup failed: {error}");
            return;
        }
    };
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break,
                    Err(error) => {
                        warn!("HTTP/3 request acceptance failed: {error}");
                        break;
                    }
                };
                let service = Arc::clone(&service);
                let shutdown = shutdown.clone();
                requests.spawn(async move {
                    handle_request(resolver, service, client_addr, shutdown).await;
                });
            }
        }
    }
    connection.close(H3_CLOSE_CODE, b"generation draining");
    while let Some(result) = requests.join_next().await {
        if let Err(error) = result {
            warn!("HTTP/3 request task failed: {error}");
        }
    }
    drop(listener_connection);
    drop(generation_reference);
    drop(service_connection);
}

async fn handle_request<C>(
    resolver: RequestResolver<C, bytes::Bytes>,
    service: Arc<ForwardHttp1ServicePlan>,
    client_addr: Option<std::net::SocketAddr>,
    shutdown: watch::Receiver<bool>,
) where
    C: h3::quic::Connection<bytes::Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<bytes::Bytes> + Send + 'static,
{
    let Ok((request, stream)) = resolver.resolve_request().await else {
        return;
    };
    service
        .handle_h3(request, stream, client_addr, shutdown)
        .await;
}

#[derive(Debug)]
struct QuicCertificateResolver {
    profile: Arc<TlsProfilePlan>,
}

impl ResolvesServerCert for QuicCertificateResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.profile
            .selected_generation(client_hello.server_name())
            .quic_certified_key()
            .map(Arc::new)
            .map_err(|error| {
                warn!("QUIC certificate selection failed: {error}");
            })
            .ok()
    }
}
