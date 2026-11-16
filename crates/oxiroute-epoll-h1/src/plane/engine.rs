use std::{
    io,
    net::SocketAddr,
    os::fd::OwnedFd,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use oxiroute_worker_scaling::{Scaling, Workers};
use rustix::net::getsockname;

use super::{
    ScalingStats,
    config::{Config, Timeouts},
    reactor::{Control, DRAINING, ListenerFd, Reactor, STOPPING},
    schedule::{Fleet, Plan},
    service::Service,
    sys,
    wake::Waker,
};

pub struct Listener<S: Service> {
    pub service: Arc<S>,
    pub timeouts: Timeouts,
    pub sockets: Vec<OwnedFd>,
    pub shared: bool,
}

impl<S: Service> Listener<S> {
    pub fn reuseport(
        address: SocketAddr,
        threads: usize,
        service: Arc<S>,
        timeouts: Timeouts,
    ) -> io::Result<Self> {
        let first = sys::bind_group(address, 1)?;
        let bound = SocketAddr::try_from(getsockname(&first[0])?)
            .map_err(|_| io::Error::other("a TCP listener has an IP address"))?;
        let mut sockets = first;
        sockets.extend(sys::bind_group(bound, threads - 1)?);
        Ok(Self {
            service,
            timeouts,
            sockets,
            shared: false,
        })
    }

    pub fn shared(
        listener: &OwnedFd,
        threads: usize,
        service: Arc<S>,
        timeouts: Timeouts,
    ) -> io::Result<Self> {
        let sockets = (0..threads)
            .map(|_| listener.try_clone())
            .collect::<io::Result<_>>()?;
        Ok(Self {
            service,
            timeouts,
            sockets,
            shared: true,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        SocketAddr::try_from(getsockname(&self.sockets[0])?)
            .map_err(|_| io::Error::other("a TCP listener has an IP address"))
    }
}

pub struct Engine {
    control: Arc<Control>,
    wakers: Vec<Waker>,
    threads: Vec<JoinHandle<io::Result<()>>>,
    exits: Mutex<mpsc::Receiver<io::Result<()>>>,
    scaling: Option<Arc<Scaling>>,
    workers: Option<Arc<dyn Workers>>,
}

struct Parts<S: Service> {
    config: Config,
    fds: Vec<ListenerFd<S>>,
    wake: Waker,
    plan: Option<Plan<S::Admission>>,
}

fn run_thread<S: Service>(
    parts: Parts<S>,
    control: Arc<Control>,
    ready: &mpsc::Sender<io::Result<()>>,
) -> io::Result<()> {
    let Parts {
        config,
        fds,
        wake,
        plan,
    } = parts;
    let reactor = match Reactor::new(&config, fds, (wake, control), plan) {
        Ok(reactor) => reactor,
        Err(error) => {
            let _ = ready.send(Err(io::Error::new(error.kind(), error.to_string())));
            return Err(error);
        }
    };
    let _ = ready.send(Ok(()));
    reactor.run()
}

fn supervised<S: Service>(
    parts: Parts<S>,
    control: Arc<Control>,
    (ready, exits): (&mpsc::Sender<io::Result<()>>, &mpsc::Sender<io::Result<()>>),
) -> io::Result<()> {
    let outcome = catch_unwind(AssertUnwindSafe(|| run_thread(parts, control, ready)))
        .unwrap_or_else(|_| Err(io::Error::other("an engine thread panicked")));
    let seen = outcome
        .as_ref()
        .copied()
        .map_err(|error| io::Error::new(error.kind(), error.to_string()));
    let _ = exits.send(seen);
    outcome
}

impl Engine {
    pub fn start<S: Service>(config: &Config, listeners: Vec<Listener<S>>) -> io::Result<Self> {
        Self::launch(config, listeners, true)
    }

    pub fn start_paused<S: Service>(
        config: &Config,
        listeners: Vec<Listener<S>>,
    ) -> io::Result<Self> {
        Self::launch(config, listeners, false)
    }

    fn launch<S: Service>(
        config: &Config,
        listeners: Vec<Listener<S>>,
        accepting: bool,
    ) -> io::Result<Self> {
        let mut per_thread = (0..config.threads).map(|_| Vec::new()).collect::<Vec<_>>();
        for listener in listeners {
            if listener.sockets.len() != config.threads {
                return Err(io::Error::other("a listener needs one socket per thread"));
            }
            for (fds, fd) in per_thread.iter_mut().zip(listener.sockets) {
                fds.push(ListenerFd {
                    service: Arc::clone(&listener.service),
                    timeouts: listener.timeouts,
                    fd,
                    shared: listener.shared,
                });
            }
        }
        let control = Arc::new(Control::new(accepting));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let wakers = (0..config.threads)
            .map(|_| Waker::new())
            .collect::<io::Result<Vec<_>>>()?;
        let fleet = config
            .scheduling
            .as_ref()
            .map(|scheduling| Fleet::<S::Admission>::new(scheduling, wakers.clone()))
            .transpose()?;
        let mut engine = Self {
            control: Arc::clone(&control),
            wakers,
            threads: Vec::new(),
            exits: Mutex::new(exit_rx),
            scaling: fleet.as_ref().and_then(Fleet::scaling),
            workers: fleet.as_ref().map(Fleet::workers),
        };
        for (index, fds) in per_thread.into_iter().enumerate() {
            let parts = Parts {
                config: config.clone(),
                fds,
                wake: engine.wakers[index].clone(),
                plan: fleet.as_ref().map(|fleet| fleet.plan(index)),
            };
            let control = Arc::clone(&control);
            let (ready, exits) = (ready_tx.clone(), exit_tx.clone());
            let handle = thread::Builder::new()
                .name(format!("epoll-h1-{index}"))
                .spawn(move || supervised(parts, control, (&ready, &exits)))?;
            engine.threads.push(handle);
        }
        drop(ready_tx);
        for _ in 0..engine.threads.len() {
            ready_rx
                .recv()
                .map_err(|_| io::Error::other("an engine thread ended at startup"))??;
        }
        if let Some(fleet) = &fleet {
            fleet.start_scaling();
        }
        Ok(engine)
    }

    /// Blocks until a thread of the engine ends with an error or a panic, or until every thread has ended cleanly.
    #[must_use]
    pub fn wait_for_failure(&self) -> Option<io::Error> {
        let exits = self.exits.lock().ok()?;
        (0..self.wakers.len()).find_map(|_| exits.recv().ok().and_then(Result::err))
    }

    /// Connections placed on each thread and not finished; empty unless the engine schedules its threads.
    #[must_use]
    pub fn connections(&self) -> Vec<usize> {
        self.workers.as_ref().map_or_else(Vec::new, |workers| {
            (0..workers.len())
                .map(|thread| workers.connections(thread))
                .collect()
        })
    }

    /// What the dynamic scaling of the threads has decided, if the engine scales them.
    #[must_use]
    pub fn scaling_stats(&self) -> Option<ScalingStats> {
        self.scaling.as_ref().map(|scaling| scaling.stats())
    }

    pub fn pause(&self) {
        self.set_accepting(false);
    }

    pub fn resume(&self) {
        self.set_accepting(true);
    }

    pub fn drain(&self) {
        self.control.raise(DRAINING);
        self.wake();
    }

    pub fn stop(&self) {
        self.control.raise(STOPPING);
        self.wake();
    }

    fn set_accepting(&self, accepting: bool) {
        self.control.set_accepting(accepting);
        self.wake();
        self.control
            .wait_armed(if accepting { self.threads.len() } else { 0 });
    }

    fn wake(&self) {
        self.wakers.iter().for_each(Waker::wake);
    }

    pub fn join(self) -> io::Result<()> {
        let joined = self.threads.into_iter().try_for_each(|thread| {
            thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("an engine thread panicked")))
        });
        if let Some(scaling) = self.scaling {
            scaling.stop();
        }
        joined
    }
}
