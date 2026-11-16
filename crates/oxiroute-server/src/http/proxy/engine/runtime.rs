use std::{io, os::fd::OwnedFd, sync::Arc};

use async_trait::async_trait;
use log::error;
use oxiroute_epoll_h1::{Config, Driver, Engine, Listener, Scheduling, TlsRouter};
use pingora::{
    apps::{AcceptGate, ServerApp},
    server::{ListenFds, RuntimeOpts, ShutdownWatch},
    services::{Service as PingoraService, ServiceReadyNotifier},
};
use tokio::{
    sync::mpsc::unbounded_channel,
    task::{JoinHandle, spawn_blocking},
};

use super::{handoff, service::EngineFront};
use crate::{DataPlane, MAX_HTTP_ATTEMPTS, RuntimeGeneration, http::proxy::lane::LaneFront};

#[derive(Clone)]
pub struct EpollLane(Arc<LaneFront>);

impl EpollLane {
    pub(crate) const fn new(front: Arc<LaneFront>) -> Self {
        Self(front)
    }
}

pub struct EpollHttpService<A> {
    name: String,
    bind: String,
    threads: usize,
    scheduling: Option<Scheduling>,
    app: Arc<A>,
    lane: Arc<LaneFront>,
    generation: Arc<RuntimeGeneration>,
    tls: Option<Arc<dyn TlsRouter>>,
    driver: Driver,
}

impl<A: ServerApp + Send + Sync + 'static> EpollHttpService<A> {
    pub fn new(
        (name, bind): (String, String),
        (threads, scheduling): (usize, Option<Scheduling>),
        app: A,
        lane: EpollLane,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            name,
            bind,
            threads,
            scheduling,
            app: Arc::new(app),
            lane: lane.0,
            generation,
            tls: None,
            driver: Driver::Epoll,
        }
    }

    #[must_use]
    pub fn terminating_tls(mut self, router: Arc<dyn TlsRouter>) -> Self {
        self.tls = Some(router);
        self
    }

    fn engine_config(&self) -> Config {
        Config {
            threads: self.threads,
            driver: self.driver,
            max_attempts: u8::try_from(MAX_HTTP_ATTEMPTS).expect("the attempt limit fits a byte"),
            drain: self.generation.retirement_deadline(),
            scheduling: self.scheduling.clone(),
            ..Config::default()
        }
    }

    #[must_use]
    pub const fn on(mut self, plane: DataPlane) -> Self {
        self.driver = match plane {
            DataPlane::IoUring => Driver::IoUring,
            DataPlane::Epoll | DataPlane::Tokio => Driver::Epoll,
        };
        self
    }

    async fn launch(&self, socket: OwnedFd, shutdown: &ShutdownWatch) -> io::Result<Engine> {
        let (handoffs, queue) = unbounded_channel();
        let front = Arc::new(EngineFront::new(
            Arc::clone(&self.app),
            Arc::clone(&self.lane),
            handoffs,
        ));
        let config = self.engine_config();
        let (threads, timeouts) = (self.threads, self.lane.engine_timeouts());
        let tls = self.tls.clone();
        let engine = spawn_blocking(move || {
            let listener = Listener::shared(&socket, threads, front, timeouts)?;
            let listener = match tls {
                Some(router) => listener.terminating_tls(router),
                None => listener,
            };
            Engine::start_paused(&config, vec![listener])
        })
        .await
        .map_err(io::Error::other)??;
        tokio::spawn(handoff::serve(
            queue,
            Arc::clone(&self.app),
            shutdown.clone(),
        ));
        Ok(engine)
    }

    async fn supervise(&self, engine: Engine, mut shutdown: ShutdownWatch) {
        let engine = Arc::new(engine);
        let mut threads = Self::watch(&engine);
        let mut running = true;
        let mut gate = self
            .app
            .accept_gate()
            .unwrap_or_else(AcceptGate::open)
            .register();
        let mut retirement = self.generation.retirement_signal();
        let mut draining = false;
        loop {
            let state = gate.state();
            if !draining {
                Self::accept(&engine, state.accepting).await;
            }
            if !state.accepting {
                gate.acknowledge(state.epoch);
            }
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = retirement.wait_for(|retired| *retired), if !draining => {
                    draining = true;
                    engine.drain();
                }
                changed = gate.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                ended = &mut threads, if running => {
                    running = false;
                    if let Some(error) = ended.unwrap_or_else(|error| Some(io::Error::other(error))) {
                        self.fail(&error);
                        break;
                    }
                }
            }
        }
        Self::stop(engine).await;
    }

    fn watch(engine: &Arc<Engine>) -> JoinHandle<Option<io::Error>> {
        let engine = Arc::clone(engine);
        spawn_blocking(move || engine.wait_for_failure())
    }

    fn fail(&self, error: &io::Error) {
        error!("listener `{}` lost an epoll thread: {error}", self.name);
        self.generation.mark_runtime_failed();
    }

    async fn accept(engine: &Arc<Engine>, accepting: bool) {
        let engine = Arc::clone(engine);
        spawn_blocking(move || {
            if accepting {
                engine.resume();
            } else {
                engine.pause();
            }
        })
        .await
        .expect("switching the engine's accepting does not panic");
    }

    async fn stop(engine: Arc<Engine>) {
        engine.stop();
        let Ok(engine) = Arc::try_unwrap(engine) else {
            return error!("the epoll data plane is still referenced at shutdown");
        };
        match spawn_blocking(move || engine.join()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!("the epoll data plane ended with an error: {error}"),
            Err(error) => error!("joining the epoll data plane failed: {error}"),
        }
    }
}

#[async_trait]
impl<A: ServerApp + Send + Sync + 'static> PingoraService for EpollHttpService<A> {
    async fn start_service_with_ready_notifier(
        &mut self,
        fds: Option<ListenFds>,
        shutdown: ShutdownWatch,
        _listeners_per_fd: usize,
        ready_notifier: ServiceReadyNotifier,
    ) {
        let Some(socket) = fds.and_then(|fds| fds.lock().take_owned(&self.bind)) else {
            return error!("listener `{}` has no socket to serve", self.name);
        };
        match self.launch(socket, &shutdown).await {
            Ok(engine) => {
                ready_notifier.notify_ready();
                self.supervise(engine, shutdown).await;
                self.app.cleanup().await;
            }
            Err(error) => error!(
                "listener `{}` could not start its epoll data plane: {error}",
                self.name
            ),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn threads(&self) -> Option<usize> {
        Some(self.threads)
    }

    fn runtime_opts_override(&self, global: &RuntimeOpts) -> Option<RuntimeOpts> {
        Some(RuntimeOpts {
            connection_age: None,
            worker_scaling: None,
            ..global.clone()
        })
    }
}
