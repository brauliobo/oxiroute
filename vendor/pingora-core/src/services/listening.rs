// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The listening service
//!
//! A [Service] (listening service) responds to incoming requests on its endpoints.
//! Each [Service] can be configured with custom application logic (e.g. an `HTTPProxy`) and one or
//! more endpoints to listen to.

use crate::apps::ServerApp;
use crate::listeners::tls::TlsSettings;
#[cfg(feature = "connection_filter")]
use crate::listeners::AcceptAllFilter;
use crate::listeners::{
    ConnectionFilter, Listeners, ServerAddress, TcpSocketOptions, TransportStack,
};
use crate::protocols::Stream;
#[cfg(unix)]
use crate::server::ListenFds;
use crate::server::ShutdownWatch;
use crate::services::{Service as ServiceTrait, ServiceReadyNotifier};

use async_trait::async_trait;
use log::{debug, error, info};
use pingora_error::Result;
use pingora_runtime::current_handle;
use pingora_timeout::timeout;
use std::fs::Permissions;
use std::sync::Arc;

/// The type of service that is associated with a list of listening endpoints and a particular application
pub struct Service<A> {
    name: String,
    listeners: Listeners,
    app_logic: Option<A>,
    /// The number of preferred threads. `None` to follow global setting.
    pub threads: Option<usize>,
    #[cfg(feature = "connection_filter")]
    connection_filter: Arc<dyn ConnectionFilter>,
}

impl<A> Service<A> {
    /// Create a new [`Service`] with the given application (see [`crate::apps`]).
    pub fn new(name: String, app_logic: A) -> Self {
        Service {
            name,
            listeners: Listeners::new(),
            app_logic: Some(app_logic),
            threads: None,
            #[cfg(feature = "connection_filter")]
            connection_filter: Arc::new(AcceptAllFilter),
        }
    }

    /// Create a new [`Service`] with the given application (see [`crate::apps`]) and the given
    /// [`Listeners`].
    pub fn with_listeners(name: String, listeners: Listeners, app_logic: A) -> Self {
        Service {
            name,
            listeners,
            app_logic: Some(app_logic),
            threads: None,
            #[cfg(feature = "connection_filter")]
            connection_filter: Arc::new(AcceptAllFilter),
        }
    }

    /// Set a custom connection filter for this service.
    ///
    /// The connection filter will be applied to all incoming connections
    /// on all endpoints of this service. Connections that don't pass the
    /// filter will be dropped immediately at the TCP level, before TLS
    /// handshake or any HTTP processing.
    ///
    /// # Feature Flag
    ///
    /// This method requires the `connection_filter` feature to be enabled.
    /// When the feature is disabled, this method is a no-op.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use pingora_core::listeners::{ConnectionFilter, AcceptAllFilter};
    /// # struct MyService;
    /// # impl MyService {
    /// #   fn new() -> Self { MyService }
    /// # }
    /// let mut service = MyService::new();
    /// let filter = Arc::new(AcceptAllFilter);
    /// service.set_connection_filter(filter);
    /// ```
    #[cfg(feature = "connection_filter")]
    pub fn set_connection_filter(&mut self, filter: Arc<dyn ConnectionFilter>) {
        self.connection_filter = filter.clone();
        self.listeners.set_connection_filter(filter);
    }

    #[cfg(not(feature = "connection_filter"))]
    pub fn set_connection_filter(&mut self, _filter: Arc<dyn ConnectionFilter>) {}

    /// Get the [`Listeners`], mostly to add more endpoints.
    pub fn endpoints(&mut self) -> &mut Listeners {
        &mut self.listeners
    }

    // the follow add* function has no effect if the server is already started

    /// Add a TCP listening endpoint with the given address (e.g., `127.0.0.1:8000`).
    pub fn add_tcp(&mut self, addr: &str) {
        self.listeners.add_tcp(addr);
    }

    /// Add a TCP listening endpoint with the given [`TcpSocketOptions`].
    pub fn add_tcp_with_settings(&mut self, addr: &str, sock_opt: TcpSocketOptions) {
        self.listeners.add_tcp_with_settings(addr, sock_opt);
    }

    /// Add a Unix domain socket listening endpoint with the given path.
    ///
    /// Optionally take a permission of the socket file. The default is read and write access for
    /// everyone (0o666).
    #[cfg(unix)]
    pub fn add_uds(&mut self, addr: &str, perm: Option<Permissions>) {
        self.listeners.add_uds(addr, perm);
    }

    /// Add a TLS listening endpoint with the given certificate and key paths.
    pub fn add_tls(&mut self, addr: &str, cert_path: &str, key_path: &str) -> Result<()> {
        self.listeners.add_tls(addr, cert_path, key_path)
    }

    /// Add a TLS listening endpoint with the given [`TlsSettings`] and [`TcpSocketOptions`].
    pub fn add_tls_with_settings(
        &mut self,
        addr: &str,
        sock_opt: Option<TcpSocketOptions>,
        settings: TlsSettings,
    ) {
        self.listeners
            .add_tls_with_settings(addr, sock_opt, settings)
    }

    /// Add an endpoint according to the given [`ServerAddress`]
    pub fn add_address(&mut self, addr: ServerAddress) {
        self.listeners.add_address(addr);
    }

    /// Get a reference to the application inside this service
    pub fn app_logic(&self) -> Option<&A> {
        self.app_logic.as_ref()
    }

    /// Get a mutable reference to the application inside this service
    pub fn app_logic_mut(&mut self) -> Option<&mut A> {
        self.app_logic.as_mut()
    }
}

impl<A: ServerApp + Send + Sync + 'static> Service<A> {
    pub async fn handle_event(event: Stream, app_logic: Arc<A>, shutdown: ShutdownWatch) {
        debug!("new event!");
        let mut reuse_event = app_logic.process_new(event, &shutdown).await;
        while let Some(event) = reuse_event {
            // TODO: with no steal runtime, consider spawn() the next event on
            // another thread for more evenly load balancing
            debug!("new reusable event!");
            reuse_event = app_logic.process_new(event, &shutdown).await;
        }
    }

    async fn run_endpoint(
        app_logic: Arc<A>,
        mut stack: TransportStack,
        mut shutdown: ShutdownWatch,
    ) {
        let mut gate = app_logic.accept_gate().map(|gate| gate.register());
        // the accept loop, until the system is shutting down
        loop {
            let new_io = if let Some(participant) = &mut gate {
                let state = participant.state();
                if !state.accepting {
                    participant.acknowledge(state.epoch);
                    tokio::select! {
                        biased;
                        shutdown_signal = shutdown.changed() => {
                            if shutdown_signal.is_err() || *shutdown.borrow() {
                                break;
                            }
                        }
                        changed = participant.changed() => {
                            if changed.is_err() {
                                break;
                            }
                        }
                    }
                    continue;
                }
                let Some(ownership) = participant.claim() else {
                    continue;
                };
                tokio::select! {
                    biased;
                    shutdown_signal = shutdown.changed() => {
                        drop(ownership);
                        match shutdown_signal {
                            Ok(()) => {
                                if !*shutdown.borrow() {
                                    continue;
                                }
                                info!("Shutting down {}", stack.as_str());
                                break;
                            }
                            Err(e) => {
                                error!("shutdown_signal error {e}");
                                break;
                            }
                        }
                    }
                    changed = participant.changed() => {
                        drop(ownership);
                        match changed {
                            Ok(state) => {
                                if !state.accepting {
                                    participant.acknowledge(state.epoch);
                                }
                                continue;
                            }
                            Err(error) => {
                                error!("accept gate error {error}");
                                break;
                            }
                        }
                    }
                    new_io = stack.accept() => {
                        let admission = app_logic.admit_owned_connection();
                        drop(ownership);
                        let Some(admission) = admission else {
                            continue;
                        };
                        match new_io {
                            Ok(io) => {
                                let app = app_logic.clone();
                                let shutdown = shutdown.clone();
                                current_handle().spawn(async move {
                                    let _admission = admission;
                                    let peer_addr = io.peer_addr();
                                    match timeout(app.handshake_timeout(), io.handshake()).await {
                                        Ok(handshake) => match handshake {
                                            Ok(io) => Self::handle_event(io, app, shutdown).await,
                                            Err(e) => {
                                                if let Some(addr) = peer_addr {
                                                    error!("Downstream handshake error from {}: {e}", addr);
                                                } else {
                                                    error!("Downstream handshake error: {e}");
                                                }
                                            }
                                        },
                                        Err(_) => error!("Downstream handshake timeout"),
                                    }
                                });
                                continue;
                            }
                            Err(error) => Err(error),
                        }
                    }
                }
            } else {
                tokio::select! { // TODO: consider biased for perf reason?
                    new_io = stack.accept() => new_io,
                    shutdown_signal = shutdown.changed() => {
                        match shutdown_signal {
                            Ok(()) => {
                                if !*shutdown.borrow() {
                                    // happen in the initial read
                                    continue;
                                }
                                info!("Shutting down {}", stack.as_str());
                                break;
                            }
                            Err(e) => {
                                error!("shutdown_signal error {e}");
                                break;
                            }
                        }
                    }
                }
            };
            match new_io {
                Ok(io) => {
                    let Some(admission) = app_logic.admit_connection() else {
                        continue;
                    };
                    let app = app_logic.clone();
                    let shutdown = shutdown.clone();
                    current_handle().spawn(async move {
                        let _admission = admission;
                        let peer_addr = io.peer_addr();
                        match timeout(app.handshake_timeout(), io.handshake()).await {
                            Ok(handshake) => {
                                match handshake {
                                    Ok(io) => Self::handle_event(io, app, shutdown).await,
                                    Err(e) => {
                                        // TODO: Maybe IOApp trait needs a fn to handle/filter out this error
                                        if let Some(addr) = peer_addr {
                                            error!("Downstream handshake error from {}: {e}", addr);
                                        } else {
                                            error!("Downstream handshake error: {e}");
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                error!("Downstream handshake timeout");
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("Accept() failed {e}");
                    if let Some(io_error) = e
                        .root_cause()
                        .downcast_ref::<std::io::Error>()
                        .and_then(|e| e.raw_os_error())
                    {
                        // 24: too many open files. In this case accept() will continue return this
                        // error without blocking, which could use up all the resources
                        if io_error == 24 {
                            // call sleep to calm the thread down and wait for others to release
                            // some resources
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }
            }
        }

        stack.cleanup();
    }

    async fn start_listeners(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: Option<ServiceReadyNotifier>,
    ) {
        if listeners_per_fd == 0 {
            error!(
                "Listening service '{}' requires at least one listener task per descriptor",
                self.name
            );
            return;
        }
        let runtime = current_handle();
        let endpoints = match self
            .listeners
            .build(
                #[cfg(unix)]
                fds,
            )
            .await
        {
            Ok(endpoints) => endpoints,
            Err(error) => {
                error!("Failed to build listeners for '{}': {error}", self.name);
                return;
            }
        };
        if endpoints.is_empty() {
            error!("Listening service '{}' has no endpoints", self.name);
            return;
        }

        let Some(app_logic) = self.app_logic.take() else {
            error!(
                "Listening service '{}' was started more than once",
                self.name
            );
            return;
        };
        let app_logic = Arc::new(app_logic);

        let mut handlers = Vec::new();
        for endpoint in endpoints {
            for _ in 0..listeners_per_fd {
                let shutdown = shutdown.clone();
                let my_app_logic = app_logic.clone();
                let endpoint = endpoint.clone();

                handlers.push(runtime.spawn(async move {
                    Self::run_endpoint(my_app_logic, endpoint, shutdown).await;
                }));
            }
        }
        if handlers.is_empty() {
            error!(
                "Listening service '{}' started no accept handlers",
                self.name
            );
            return;
        }
        if let Some(ready_notifier) = ready_notifier {
            ready_notifier.notify_ready();
        }

        futures::future::join_all(handlers).await;
        self.listeners.cleanup();
        app_logic.cleanup().await;
    }
}

#[async_trait]
impl<A: ServerApp + Send + Sync + 'static> ServiceTrait for Service<A> {
    async fn start_service_with_ready_notifier(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        self.start_listeners(
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
            Some(ready_notifier),
        )
        .await;
    }

    async fn start_service(
        &mut self,
        #[cfg(unix)] fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        listeners_per_fd: usize,
    ) {
        self.start_listeners(
            #[cfg(unix)]
            fds,
            shutdown,
            listeners_per_fd,
            None,
        )
        .await;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn threads(&self) -> Option<usize> {
        self.threads
    }
}

use crate::apps::prometheus_http_app::PrometheusServer;

impl Service<PrometheusServer> {
    /// The Prometheus HTTP server
    ///
    /// The HTTP server endpoint that reports Prometheus metrics collected in the entire service
    pub fn prometheus_http_service() -> Self {
        Service::new(
            "Prometheus metric HTTP".to_string(),
            PrometheusServer::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::ServerApp;
    use crate::protocols::Stream;

    struct TestApp;

    #[async_trait]
    impl ServerApp for TestApp {
        async fn process_new(
            self: &Arc<Self>,
            _session: Stream,
            _shutdown: &ShutdownWatch,
        ) -> Option<Stream> {
            None
        }
    }

    #[test]
    fn zero_listener_tasks_never_publish_readiness() {
        tokio_test::block_on(async {
            let mut service = Service::new("zero listeners".into(), TestApp);
            let (_shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
            let (ready_sender, ready_watch) = tokio::sync::watch::channel(false);

            ServiceTrait::start_service_with_ready_notifier(
                &mut service,
                #[cfg(unix)]
                None,
                shutdown,
                0,
                ServiceReadyNotifier::new(ready_sender).require_explicit(),
            )
            .await;

            assert!(!*ready_watch.borrow());
            assert!(ready_watch.has_changed().is_err());
        });
    }

    #[test]
    fn empty_endpoints_never_publish_readiness_with_a_valid_multiplier() {
        tokio_test::block_on(async {
            let mut service = Service::new("empty endpoints".into(), TestApp);
            let (_shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
            let (ready_sender, ready_watch) = tokio::sync::watch::channel(false);

            ServiceTrait::start_service_with_ready_notifier(
                &mut service,
                #[cfg(unix)]
                None,
                shutdown,
                1,
                ServiceReadyNotifier::new(ready_sender).require_explicit(),
            )
            .await;

            assert!(service.app_logic().is_some());
            assert!(!*ready_watch.borrow());
            assert!(ready_watch.has_changed().is_err());
        });
    }
}
