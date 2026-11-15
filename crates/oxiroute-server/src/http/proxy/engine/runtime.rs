use std::{io, os::fd::OwnedFd, sync::Arc};

use async_trait::async_trait;
use log::error;
use oxiroute_epoll_h1::{Config, Engine, Listener};
use pingora::{
    apps::{AcceptGate, ServerApp},
    server::{ListenFds, ShutdownWatch},
    services::{Service as PingoraService, ServiceReadyNotifier},
};
use tokio::{sync::mpsc::unbounded_channel, task::spawn_blocking};

use super::{handoff, service::EngineFront};
use crate::{MAX_HTTP_ATTEMPTS, RuntimeGeneration, http::proxy::lane::LaneFront};

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
    app: Arc<A>,
    lane: Arc<LaneFront>,
    generation: Arc<RuntimeGeneration>,
}

impl<A: ServerApp + Send + Sync + 'static> EpollHttpService<A> {
    pub fn new(
        (name, bind): (String, String),
        threads: usize,
        app: A,
        lane: EpollLane,
        generation: Arc<RuntimeGeneration>,
    ) -> Self {
        Self {
            name,
            bind,
            threads,
            app: Arc::new(app),
            lane: lane.0,
            generation,
        }
    }

    async fn launch(&self, socket: OwnedFd, shutdown: &ShutdownWatch) -> io::Result<Engine> {
        let (handoffs, queue) = unbounded_channel();
        let front = Arc::new(EngineFront::new(
            Arc::clone(&self.app),
            Arc::clone(&self.lane),
            handoffs,
        ));
        let config = Config {
            threads: self.threads,
            max_attempts: u8::try_from(MAX_HTTP_ATTEMPTS).expect("the attempt limit fits a byte"),
            ..Config::default()
        };
        let (threads, timeouts) = (self.threads, self.lane.engine_timeouts());
        let engine = spawn_blocking(move || {
            let listener = Listener::shared(&socket, threads, front, timeouts)?;
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
            }
        }
        Self::stop(engine).await;
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
}
