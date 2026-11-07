#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct TcpRelay {
    generation: Arc<RuntimeGeneration>,
    listener: ListenerRuntime,
    service: Arc<oxiroute_server::L4ServicePlan>,
    metrics: ListenerMetrics,
    proxy_protocol: Option<oxiroute_config::ProxyProtocolPolicy>,
}

pub(super) struct ForwardHttp1App {
    metrics: ListenerMetrics,
    require_h1_alpn: bool,
    request_timeout: Option<Duration>,
    service: Arc<ForwardHttp1ServicePlan>,
    challenge_store: oxiroute_acme::ChallengeStore,
}

pub(super) struct ForwardHttp1RuntimeApp {
    inner: Arc<MonitoredHttpApp<ForwardHttp1App>>,
    handshake_timeout: Duration,
}

pub(super) struct ForwardHttp2App<A> {
    generation: Arc<RuntimeGeneration>,
    inner: Arc<A>,
    listener: ListenerRuntime,
    service: Arc<ForwardHttp1ServicePlan>,
}

impl<A> ForwardHttp2App<A> {
    pub(super) fn new(
        service: Arc<ForwardHttp1ServicePlan>,
        inner: A,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            generation,
            inner: Arc::new(inner),
            listener: ListenerRuntime::new(metrics),
            service,
        }
    }
}

struct ForwardDownstream<S> {
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    inner: S,
    metrics: ConnectionGuard,
}

impl<S> ForwardDownstream<S> {
    fn new(inner: S, idle_timeout: Duration, metrics: ConnectionGuard) -> Self {
        Self {
            idle: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            inner,
            metrics,
        }
    }

    fn reset_idle(&mut self) {
        self.idle.as_mut().reset(Instant::now() + self.idle_timeout);
    }

    fn poll_idle(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.idle.as_mut().poll(context).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "forward downstream idle timeout",
            ))
        } else {
            Ok(())
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ForwardDownstream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = &result {
            let read = buffer.filled().len() - before;
            if read > 0 {
                if let Err(error) = this
                    .metrics
                    .record_bytes_received(u64::try_from(read).unwrap_or(u64::MAX))
                {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                this.reset_idle();
            }
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ForwardDownstream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut this.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(written)) = result {
            if written > 0 {
                if let Err(error) = this
                    .metrics
                    .record_bytes_sent(u64::try_from(written).unwrap_or(u64::MAX))
                {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                this.reset_idle();
            }
            Poll::Ready(Ok(written))
        } else {
            result
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_idle(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

impl ForwardHttp1App {
    pub(super) fn new(
        service: Arc<ForwardHttp1ServicePlan>,
        metrics: ListenerMetrics,
        request_timeout: Option<Duration>,
        require_h1_alpn: bool,
        challenge_store: oxiroute_acme::ChallengeStore,
    ) -> Self {
        Self {
            metrics,
            require_h1_alpn,
            request_timeout,
            service,
            challenge_store,
        }
    }
}

impl ForwardHttp1RuntimeApp {
    pub(super) fn new(
        inner: ForwardHttp1App,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        let handshake_timeout = inner.handshake_timeout();
        Self {
            inner: Arc::new(MonitoredHttpApp::new(inner, metrics, generation)),
            handshake_timeout,
        }
    }
}

#[async_trait]
impl ServerApp for ForwardHttp1App {
    fn handshake_timeout(&self) -> Duration {
        self.service
            .idle_timeout()
            .min(self.service.lifetime_timeout())
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        Some(Box::new(service))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        Some(Box::new(service))
    }

    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        if self.require_h1_alpn && !matches!(downstream.selected_alpn_proto(), Some(ALPN::H1)) {
            // A TLS forward-HTTP/1 listener must not accept an unnegotiated protocol.
            let _ = downstream.shutdown().await;
            return None;
        }
        let client_addr = downstream.get_socket_digest().and_then(|digest| {
            digest
                .peer_addr()
                .and_then(|address| address.as_inet().copied())
        });
        let plan = Arc::clone(&self.service);
        let request_shutdown = shutdown.clone();
        let lifecycle = Arc::new(ForwardConnectionLifecycle::default());
        let request_lifecycle = Arc::clone(&lifecycle);
        let challenge_store = self.challenge_store.clone();
        let request_metrics = self.metrics.clone();
        let app = service_fn(move |request| {
            let plan = Arc::clone(&plan);
            let shutdown = request_shutdown.clone();
            let lifecycle = Arc::clone(&request_lifecycle);
            let challenge_store = challenge_store.clone();
            let metrics = request_metrics.clone();
            async move {
                let response = match oxiroute_server::challenge_response(&request, &challenge_store)
                {
                    Some(response) => response,
                    None => {
                        plan.handle(request, client_addr, shutdown, lifecycle, metrics)
                            .await
                    }
                };
                Ok::<_, Infallible>(response)
            }
        });
        let mut builder = http1::Builder::new();
        builder.max_buf_size(self.service.max_header_bytes());
        let header_timeout = self
            .request_timeout
            .map_or(self.service.idle_timeout(), |timeout| {
                timeout.min(self.service.idle_timeout())
            });
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(header_timeout);
        let downstream = ForwardDownstream::new(
            downstream,
            self.service.idle_timeout(),
            self.metrics.traffic_accounting(),
        );
        let connection = builder
            .serve_connection(TokioIo::new(downstream), app)
            .with_upgrades();
        tokio::pin!(connection);
        let lifetime = tokio::time::sleep(self.service.lifetime_timeout());
        tokio::pin!(lifetime);
        let mut shutdown = shutdown.clone();
        tokio::select! {
            result = &mut connection => {
                if let Err(error) = result {
                    warn!("forward HTTP/1 connection failed: {error}");
                }
            }
            () = &mut lifetime => {}
            _ = shutdown.changed() => {}
        }
        lifecycle.wait_if_started().await;
        None
    }
}

#[async_trait]
impl ServerApp for ForwardHttp1RuntimeApp {
    fn accept_gate(&self) -> Option<AcceptGate> {
        self.inner.accept_gate()
    }

    fn accepting(&self) -> bool {
        self.inner.accepting()
    }

    fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_connection()
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        self.inner.admit_owned_connection()
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

#[async_trait]
impl<A> ServerApp for ForwardHttp2App<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.listener.accepting() && self.generation.accepting() && self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let lease = match self
            .listener
            .admit(&self.generation, RuntimeReferenceKind::Http2)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected forward HTTP/2 connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((service, lease, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let Some(service) = self.service.begin_connection() else {
            warn!(
                "rejected connection on forward service `{}`: service connection limit reached",
                self.service.name()
            );
            return None;
        };
        let lease = match self
            .listener
            .admit_owned(&self.generation, RuntimeReferenceKind::Http2)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected forward HTTP/2 connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((service, lease, inner)))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.inner.process_new(downstream, shutdown).await
    }

    async fn cleanup(&self) {
        self.inner.cleanup().await;
    }
}

impl TcpRelay {
    pub(super) fn new(
        service: Arc<oxiroute_server::L4ServicePlan>,
        metrics: ListenerMetrics,
        proxy_protocol: Option<oxiroute_config::ProxyProtocolPolicy>,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            generation,
            listener: ListenerRuntime::new(metrics.clone()),
            service,
            metrics,
            proxy_protocol,
        }
    }
}

#[async_trait]
impl ServerApp for TcpRelay {
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        self.listener.accepting() && self.generation.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit(&self.generation, RuntimeReferenceKind::Tcp)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected TCP connection: {error}");
                return None;
            }
        };
        Some(Box::new(lease))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit_owned(&self.generation, RuntimeReferenceKind::Tcp)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected TCP connection: {error}");
                return None;
            }
        };
        Some(Box::new(lease))
    }

    async fn process_new(
        self: &Arc<Self>,
        downstream: Stream,
        shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let physical_client_address = downstream.get_socket_digest().and_then(|digest| {
            digest
                .peer_addr()
                .and_then(|address| address.as_inet().copied())
        });
        let connection = self.metrics.traffic_accounting();
        if let Some(policy) = self.proxy_protocol {
            let mut proxy_shutdown = shutdown.clone();
            let accepted =
                match oxiroute_server::accept_stream(downstream, policy, &mut proxy_shutdown).await
                {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        if let Err(metric_error) =
                            self.metrics.record_proxy_protocol(error.result())
                        {
                            debug!(
                                "could not account for TCP PROXY protocol rejection: {metric_error}"
                            );
                        }
                        warn!("TCP PROXY protocol rejected: {error}");
                        return None;
                    }
                };
            if let Err(metric_error) = self
                .metrics
                .record_proxy_protocol(oxiroute_server::ProxyProtocolResult::Accepted)
            {
                debug!("could not account for TCP PROXY protocol: {metric_error}");
            }
            let Some(upstream) =
                oxiroute_server::select_upstream_with_shutdown(&self.service, shutdown).await
            else {
                warn!("TCP pool has no healthy upstream");
                return None;
            };
            let relay = TcpRelayCore::new(upstream, self.service.policy())
                .with_proxy_protocol(self.service.proxy_protocol(), Some(accepted.header.source));
            if let Err(error) = relay
                .relay(accepted.stream, &connection, shutdown.clone())
                .await
            {
                warn!("TCP relay failed: {error}");
            }
        } else {
            let Some(upstream) =
                oxiroute_server::select_upstream_with_shutdown(&self.service, shutdown).await
            else {
                warn!("TCP pool has no healthy upstream");
                return None;
            };
            let relay = TcpRelayCore::new(upstream, self.service.policy())
                .with_proxy_protocol(self.service.proxy_protocol(), physical_client_address);
            if let Err(error) = relay.relay(downstream, &connection, shutdown.clone()).await {
                warn!("TCP relay failed: {error}");
            }
        }

        None
    }
}
