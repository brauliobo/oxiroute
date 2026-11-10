#[allow(clippy::wildcard_imports)]
use super::*;

const TEST_LISTENER_DUPLICATION_FAILURE_ENV: &str =
    "OXIROUTE_INTERNAL_TEST_LISTENER_DUPLICATION_FAILURE";

const TEST_AGED_PER_WORKER_ENV: &str = "OXIROUTE_INTERNAL_TEST_AGED_PER_WORKER";

/// Whether the workers of a listener serve per-worker queues. The test variable runs a whole process suite in that
/// mode.
pub(super) fn aged_per_worker(config: &oxiroute_config::ConfigDraft) -> bool {
    config.aged_per_worker() || std::env::var_os(TEST_AGED_PER_WORKER_ENV).is_some()
}

/// Worker threads for each HTTP, forward HTTP/1, forward HTTP/2 and TCP listener.
pub(super) fn listener_workers(config: &oxiroute_config::ConfigDraft) -> usize {
    config.listener_workers.map_or(1, |workers| {
        usize::try_from(workers).expect("validated worker count fits usize")
    })
}

/// Threads that run the TLS handshakes of every TLS listener (0: on the listener workers).
pub(super) fn tls_handshake_threads(config: &oxiroute_config::ConfigDraft) -> usize {
    config.effective_tls_handshake_threads(
        std::thread::available_parallelism().map_or(1, usize::from),
    )
}

pub(super) fn add_plain_listener<A>(
    service: &mut Service<A>,
    listener_name: &str,
    bind: &ListenerBind,
) -> io::Result<()> {
    match bind {
        ListenerBind::Socket { address } => {
            service.add_tcp(&address.to_string());
            Ok(())
        }
        ListenerBind::Udp { address } => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("listener `{listener_name}` cannot register UDP socket `{address}` yet"),
        )),
        ListenerBind::Unix { path, mode } => {
            let path = path.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "listener `{listener_name}` Unix socket path is not valid UTF-8 and cannot be registered"
                    ),
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;

                service.add_uds(
                    path,
                    mode.map(|mode| std::fs::Permissions::from_mode(u32::from(mode))),
                );
                Ok(())
            }
            #[cfg(not(unix))]
            {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "listener `{listener_name}` cannot register Unix socket `{path}` on this platform"
                    ),
                ))
            }
        }
    }
}

pub(super) struct RuntimeListenerService<S> {
    generation: Option<Arc<RuntimeGeneration>>,
    inner: S,
    metrics: Option<ListenerMetrics>,
    reservation: ListenerReservation,
}

impl<S> RuntimeListenerService<S> {
    pub(super) fn new(
        inner: S,
        reservation: ListenerReservation,
        metrics: Option<ListenerMetrics>,
    ) -> Self {
        Self {
            generation: None,
            inner,
            metrics,
            reservation,
        }
    }

    pub(super) fn with_generation(mut self, generation: Arc<RuntimeGeneration>) -> Self {
        self.generation = Some(generation);
        self
    }
}

#[async_trait]
impl<S> ServiceWithDependents for RuntimeListenerService<S>
where
    S: PingoraService + Send + Sync,
{
    async fn start_service(
        &mut self,
        #[cfg(unix)] _inherited_fds: Option<pingora::server::ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        let shutdown_status = shutdown.clone();
        let service_name = self.inner.name().to_owned();
        let ready_notifier = ready_notifier.require_explicit();
        #[cfg(unix)]
        let duplicated = if cfg!(target_os = "linux")
            && std::env::var_os(TEST_LISTENER_DUPLICATION_FAILURE_ENV).is_some()
        {
            Err(io::Error::other("injected listener duplication failure"))
        } else {
            self.reservation.duplicate_fds()
        };
        #[cfg(unix)]
        let fds = match duplicated {
            Ok(fds) => fds,
            Err(error) => {
                if let Some(metrics) = &self.metrics {
                    metrics.mark_failed();
                }
                if let Some(generation) = &self.generation {
                    generation.mark_runtime_failed();
                }
                error!("listener service `{service_name}` descriptor duplication failed: {error}");
                return;
            }
        };
        #[cfg(unix)]
        let fds = Some(Arc::new(parking_lot::Mutex::new(fds)));
        let ready_metrics = self.metrics.clone();
        let ready_notifier = ready_notifier.with_ready_callback(move || {
            if let Some(metrics) = ready_metrics {
                metrics.mark_listening();
            }
        });
        let result = AssertUnwindSafe(PingoraService::start_service_with_ready_notifier(
            &mut self.inner,
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
            ready_notifier,
        ))
        .catch_unwind()
        .await;
        let unexpected = !*shutdown_status.borrow();
        if let Some(metrics) = &self.metrics {
            if result.is_ok() && !unexpected {
                metrics.mark_stopped();
            } else {
                metrics.mark_failed();
            }
        }
        if result.is_err() || unexpected {
            if let Some(generation) = &self.generation {
                generation.mark_runtime_failed();
            }
            error!("listener service `{service_name}` terminated unexpectedly");
        }
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn threads(&self) -> Option<usize> {
        self.inner.threads()
    }
}

pub(super) struct GenerationRuntimeMarker {
    pub(super) generation: Arc<RuntimeGeneration>,
}

pub(super) struct GenerationBackgroundService<S> {
    pub(super) _generation: Arc<RuntimeGeneration>,
    pub(super) inner: S,
}

#[async_trait]
impl<S> pingora::services::background::BackgroundService for GenerationBackgroundService<S>
where
    S: pingora::services::background::BackgroundService + Sync,
{
    async fn start_with_ready_notifier(
        &self,
        shutdown: ShutdownWatch,
        ready_notifier: ServiceReadyNotifier,
    ) {
        self.inner
            .start_with_ready_notifier(shutdown, ready_notifier)
            .await;
    }
}

#[async_trait]
impl ServiceWithDependents for GenerationRuntimeMarker {
    async fn start_service(
        &mut self,
        #[cfg(unix)] _fds: Option<pingora::server::ListenFds>,
        mut shutdown: ShutdownWatch,
        _listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        if !self.generation.mark_runtime_started() {
            return;
        }
        ready_notifier.notify_ready();
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
    }

    fn name(&self) -> &'static str {
        "OxiRoute generation runtime marker"
    }
}

pub(super) fn add_http_listener<A>(
    service: &mut Service<A>,
    listener_name: &str,
    bind: &ListenerBind,
    tls: Option<&TlsProfilePlan>,
    handshake_threads: usize,
) -> Result<(), Box<dyn Error>> {
    match (bind, tls) {
        (ListenerBind::Socket { address }, Some(tls)) => {
            service.add_tls_with_settings(
                &address.to_string(),
                None,
                tls.tls_settings(handshake_threads)?,
            );
            Ok(())
        }
        (ListenerBind::Udp { .. }, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("listener `{listener_name}` cannot register stream TLS on a UDP socket"),
        )
        .into()),
        (ListenerBind::Unix { .. }, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("listener `{listener_name}` cannot register TLS on a Unix socket"),
        )
        .into()),
        (_, None) => {
            add_plain_listener(service, listener_name, bind)?;
            Ok(())
        }
    }
}

pub(super) struct ProcessAdmissionApp<A> {
    generation: Arc<RuntimeGeneration>,
    inner: Arc<A>,
    listener: ListenerRuntime,
}

impl<A> ProcessAdmissionApp<A> {
    pub(super) fn new(
        inner: A,
        metrics: ListenerMetrics,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            generation,
            inner: Arc::new(inner),
            listener: ListenerRuntime::new(metrics),
        }
    }
}

#[async_trait]
impl<A> ServerApp for ProcessAdmissionApp<A>
where
    A: ServerApp + Send + Sync + 'static,
{
    fn accept_gate(&self) -> Option<AcceptGate> {
        Some(self.generation.accept_gate())
    }

    fn accepting(&self) -> bool {
        // Control listeners intentionally stay reachable during process administrative drain.
        self.generation.accepting() && self.inner.accepting()
    }

    fn admit_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit_control(&self.generation, RuntimeReferenceKind::Http1)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_connection()?;
        Some(Box::new((lease, inner)))
    }

    fn admit_owned_connection(&self) -> Option<ConnectionAdmission> {
        let lease = match self
            .listener
            .admit_owned_control(&self.generation, RuntimeReferenceKind::Http1)
        {
            Ok(lease) => lease,
            Err(error) => {
                warn!("rejected management connection: {error}");
                return None;
            }
        };
        let inner = self.inner.admit_owned_connection()?;
        Some(Box::new((lease, inner)))
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
