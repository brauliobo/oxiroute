use std::{
    io,
    net::SocketAddr,
    os::fd::OwnedFd,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use rustix::net::getsockname;

use super::{
    config::{Config, Timeouts},
    reactor::{Control, DRAINING, ListenerFd, Reactor, STOPPING},
    service::Service,
    sys,
    wake::Waker,
};

pub struct Listener<S: Service> {
    pub service: Arc<S>,
    pub timeouts: Timeouts,
    pub sockets: Vec<OwnedFd>,
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
}

fn run_thread<S: Service>(
    (config, fds, wake): (Config, Vec<ListenerFd<S>>, Waker),
    control: Arc<Control>,
    ready: &mpsc::Sender<io::Result<()>>,
) -> io::Result<()> {
    let reactor = match Reactor::new(&config, fds, wake, control) {
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
    parts: (Config, Vec<ListenerFd<S>>, Waker),
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
                });
            }
        }
        let control = Arc::new(Control::new(accepting));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let mut engine = Self {
            control: Arc::clone(&control),
            wakers: Vec::new(),
            threads: Vec::new(),
            exits: Mutex::new(exit_rx),
        };
        for (index, fds) in per_thread.into_iter().enumerate() {
            let wake = Waker::new()?;
            engine.wakers.push(wake.clone());
            let (config, control) = (config.clone(), Arc::clone(&control));
            let (ready, exits) = (ready_tx.clone(), exit_tx.clone());
            let handle = thread::Builder::new()
                .name(format!("epoll-h1-{index}"))
                .spawn(move || supervised((config, fds, wake), control, (&ready, &exits)))?;
            engine.threads.push(handle);
        }
        drop(ready_tx);
        for _ in 0..engine.threads.len() {
            ready_rx
                .recv()
                .map_err(|_| io::Error::other("an engine thread ended at startup"))??;
        }
        Ok(engine)
    }

    /// Blocks until a thread of the engine ends with an error or a panic, or until every thread has ended cleanly.
    #[must_use]
    pub fn wait_for_failure(&self) -> Option<io::Error> {
        let exits = self.exits.lock().ok()?;
        (0..self.wakers.len()).find_map(|_| exits.recv().ok().and_then(Result::err))
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
        self.threads.into_iter().try_for_each(|thread| {
            thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("an engine thread panicked")))
        })
    }
}
