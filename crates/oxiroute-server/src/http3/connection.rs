#[allow(clippy::wildcard_imports)]
use super::*;

/// Accepts a connection of the shared endpoint under the configuration of the accepting generation.
async fn accept_incoming(
    incoming: quinn::Incoming,
    server_config: Arc<ServerConfig>,
) -> Result<quinn::Connection, quinn::ConnectionError> {
    incoming.accept_with(server_config)?.await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_reverse_connection(
    incoming: quinn::Incoming,
    server_config: Arc<ServerConfig>,
    service: Arc<HttpServicePlan>,
    generation: Arc<RuntimeGeneration>,
    listener: ListenerRuntime,
    metrics: ListenerMetrics,
    downstream_timeouts: DownstreamTimeoutPolicy,
    mut shutdown: watch::Receiver<bool>,
    mut drain: watch::Receiver<bool>,
    _handshake: OwnedSemaphorePermit,
) {
    let connection = match accept_incoming(incoming, server_config).await {
        Ok(connection) => connection,
        Err(error) => {
            warn!("reverse HTTP/3 QUIC handshake failed: {error}");
            return;
        }
    };
    let traffic_lease =
        match listener.admit_runtime_connection(&generation, RuntimeReferenceKind::Http3) {
            Ok(lease) => lease,
            Err(error) => {
                let reason = match &error {
                    AdmissionError::GenerationNotAccepting => b"generation draining".as_slice(),
                    AdmissionError::Metrics(_) => b"listener connection limit".as_slice(),
                };
                warn!("reverse HTTP/3 connection admission failed: {error}");
                connection.close(H3_CLOSE_CODE, reason);
                return;
            }
        };
    let mut builder = h3::server::builder();
    builder
        .max_field_section_size(H3_MAX_FIELD_SECTION_BYTES)
        .send_grease(false);
    let mut h3: h3::server::Connection<_, Bytes> = match builder
        .build(h3_quinn::Connection::new(connection.clone()))
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            warn!("reverse HTTP/3 connection setup failed: {error}");
            return;
        }
    };
    let request_slots = Arc::new(Semaphore::new(H3_BIDI_STREAM_LIMIT as usize));
    let mut requests = tokio::task::JoinSet::new();
    let (request_cancel_tx, request_cancel_rx) = watch::channel(false);
    let client_addr = Some(connection.remote_address());
    let graceful = loop {
        tokio::select! {
            _ = drain.changed() => break true,
            _ = shutdown.changed() => break true,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break false,
                    Err(error) => {
                        warn!("reverse HTTP/3 request acceptance failed: {error}");
                        break false;
                    }
                };
                if *drain.borrow() || *shutdown.borrow() {
                    reject_h3_resolver(resolver);
                    break true;
                }
                let service = Arc::clone(&service);
                let metrics = metrics.clone();
                let request_cancel = request_cancel_rx.clone();
                let request_slots = Arc::clone(&request_slots);
                requests.spawn(async move {
                    Box::pin(handle_reverse_request(
                        resolver,
                        service,
                        metrics,
                        client_addr,
                        downstream_timeouts,
                        request_cancel,
                        request_slots,
                    ))
                    .await;
                });
            }
        }
    };
    let deadline = Instant::now() + h3_drain_window(&generation, &shutdown);
    let goaway_sent = if graceful {
        drain_h3_connection(&mut h3, deadline).await
    } else {
        false
    };
    if !goaway_sent {
        let _ = request_cancel_tx.send(true);
        requests.abort_all();
    }
    join_h3_requests(
        "reverse HTTP/3",
        &mut requests,
        &request_cancel_tx,
        deadline,
    )
    .await;
    connection.close(H3_CLOSE_CODE, b"generation draining");
    drop(traffic_lease);
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_connection(
    incoming: quinn::Incoming,
    server_config: Arc<ServerConfig>,
    service: Arc<ForwardHttp1ServicePlan>,
    generation: Arc<RuntimeGeneration>,
    listener: ListenerRuntime,
    metrics: ListenerMetrics,
    mut shutdown: watch::Receiver<bool>,
    mut drain: watch::Receiver<bool>,
    _handshake: tokio::sync::OwnedSemaphorePermit,
) {
    let connection = match accept_incoming(incoming, server_config).await {
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
    let traffic_lease =
        match listener.admit_runtime_connection(&generation, RuntimeReferenceKind::ForwardHttp3) {
            Ok(lease) => lease,
            Err(error) => {
                let reason = match &error {
                    AdmissionError::GenerationNotAccepting => b"generation draining".as_slice(),
                    AdmissionError::Metrics(_) => b"listener connection limit".as_slice(),
                };
                warn!("HTTP/3 connection admission failed: {error}");
                connection.close(H3_CLOSE_CODE, reason);
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
    let (request_cancel_tx, request_cancel_rx) = watch::channel(false);
    let graceful = loop {
        tokio::select! {
            _ = drain.changed() => break true,
            _ = shutdown.changed() => break true,
            accepted = h3.accept() => {
                let resolver = match accepted {
                    Ok(Some(resolver)) => resolver,
                    Ok(None) => break false,
                    Err(error) => {
                        warn!("HTTP/3 request acceptance failed: {error}");
                        break false;
                    }
                };
                if *drain.borrow() || *shutdown.borrow() {
                    reject_h3_resolver(resolver);
                    break true;
                }
                let service = Arc::clone(&service);
                let metrics = metrics.clone();
                let request_cancel = request_cancel_rx.clone();
                requests.spawn(async move {
                    handle_request(resolver, service, client_addr, request_cancel, metrics).await;
                });
            }
        }
    };
    let deadline = Instant::now() + h3_drain_window(&generation, &shutdown);
    let goaway_sent = if graceful {
        drain_h3_connection(&mut h3, deadline).await
    } else {
        false
    };
    if !goaway_sent {
        let _ = request_cancel_tx.send(true);
        requests.abort_all();
    }
    join_h3_requests("HTTP/3", &mut requests, &request_cancel_tx, deadline).await;
    connection.close(H3_CLOSE_CODE, b"generation draining");
    drop(traffic_lease);
    drop(service_connection);
}

async fn drain_h3_connection<C>(
    connection: &mut h3::server::Connection<C, Bytes>,
    deadline: Instant,
) -> bool
where
    C: h3::quic::Connection<Bytes>,
{
    if !matches!(
        timeout_at(deadline, connection.shutdown(0)).await,
        Ok(Ok(()))
    ) {
        return false;
    }
    true
}

fn reject_h3_resolver<C>(mut resolver: RequestResolver<C, Bytes>)
where
    C: h3::quic::Connection<Bytes>,
{
    let code = h3::error::Code::H3_REQUEST_REJECTED.value();
    resolver.frame_stream.reset(code);
    resolver.frame_stream.stream.stop_sending(code);
}

pub(super) async fn join_h3_requests(
    protocol: &str,
    requests: &mut tokio::task::JoinSet<()>,
    request_cancel: &watch::Sender<bool>,
    deadline: Instant,
) {
    let mut timed_out = false;
    loop {
        match timeout_at(deadline, requests.join_next()).await {
            Ok(Some(result)) => {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    warn!("{protocol} request task failed: {error}");
                }
            }
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                warn!("{protocol} request drain deadline expired");
                break;
            }
        }
    }
    if timed_out {
        let _ = request_cancel.send(true);
        requests.abort_all();
        while let Some(result) = requests.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                warn!("{protocol} request task failed: {error}");
            }
        }
    }
}

async fn handle_request<C>(
    resolver: RequestResolver<C, bytes::Bytes>,
    service: Arc<ForwardHttp1ServicePlan>,
    client_addr: Option<std::net::SocketAddr>,
    shutdown: watch::Receiver<bool>,
    metrics: ListenerMetrics,
) where
    C: h3::quic::Connection<bytes::Bytes> + Send + 'static,
    C::BidiStream: h3::quic::BidiStream<bytes::Bytes> + Send + 'static,
{
    let Ok((request, stream)) = resolver.resolve_request().await else {
        return;
    };
    service
        .handle_h3(request, stream, client_addr, shutdown, metrics)
        .await;
}
