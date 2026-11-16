use std::{
    io,
    net::SocketAddr,
    os::fd::OwnedFd,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
};

use rustix::{
    event::{EventfdFlags, eventfd},
    net::getsockname,
};

use super::{
    config::{Config, Timeouts},
    reactor::{Control, DRAINING, ListenerFd, Reactor, STOPPING},
    service::Service,
    sys,
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
    wakers: Vec<OwnedFd>,
    threads: Vec<JoinHandle<io::Result<()>>>,
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
        let mut engine = Self {
            control: Arc::clone(&control),
            wakers: Vec::new(),
            threads: Vec::new(),
        };
        for (index, fds) in per_thread.into_iter().enumerate() {
            let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
            engine.wakers.push(wake.try_clone()?);
            let (config, control, ready) = (config.clone(), Arc::clone(&control), ready_tx.clone());
            let handle = thread::Builder::new()
                .name(format!("{}-h1-{index}", config.driver.thread_prefix()))
                .spawn(move || {
                    let reactor = match Reactor::new(&config, fds, wake, control) {
                        Ok(reactor) => reactor,
                        Err(error) => {
                            let _ =
                                ready.send(Err(io::Error::new(error.kind(), error.to_string())));
                            return Err(error);
                        }
                    };
                    let _ = ready.send(Ok(()));
                    reactor.run()
                })?;
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
        for wake in &self.wakers {
            let _ = rustix::io::write(wake, &1_u64.to_ne_bytes());
        }
    }

    pub fn join(self) -> io::Result<()> {
        self.threads.into_iter().try_for_each(|thread| {
            thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("an engine thread panicked")))
        })
    }
}
