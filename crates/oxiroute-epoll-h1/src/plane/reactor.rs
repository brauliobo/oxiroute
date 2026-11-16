use std::{
    io,
    os::fd::OwnedFd,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use rustix::event::epoll::EventFlags;

use super::{
    client::Shared,
    clock::{Clock, Moment},
    config::{Config, Timeouts},
    driver::{Driver, Polled},
    origin_head::Heads,
    pipes::Pipes,
    pool::BufferPool,
    schedule::{Plan, Schedule},
    service::Service,
    slab::{Body, Slab, Token},
    spare::Spare,
    sys,
    tls::TlsRouter,
    upstream::IdlePool,
    wake::Waker,
    wheel::Wheel,
};

pub(crate) const RUNNING: u8 = 0;
pub(crate) const DRAINING: u8 = 1;
pub(crate) const STOPPING: u8 = 2;

pub(crate) struct Control {
    state: AtomicU8,
    accepting: AtomicBool,
    armed: Mutex<usize>,
    changed: Condvar,
}

impl Control {
    pub(crate) fn new(accepting: bool) -> Self {
        Self {
            state: AtomicU8::new(RUNNING),
            accepting: AtomicBool::new(accepting),
            armed: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }

    pub(crate) fn raise(&self, state: u8) {
        self.state.fetch_max(state, Ordering::Relaxed);
        self.announce();
    }

    pub(crate) fn accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    pub(crate) fn set_accepting(&self, accepting: bool) {
        self.accepting.store(accepting, Ordering::Release);
    }

    fn armed(&self) -> MutexGuard<'_, usize> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn announce(&self) {
        drop(self.armed());
        self.changed.notify_all();
    }

    pub(crate) fn arm(&self) {
        *self.armed() += 1;
        self.changed.notify_all();
    }

    pub(crate) fn disarm(&self) {
        *self.armed() -= 1;
        self.changed.notify_all();
    }

    pub(crate) fn wait_armed(&self, threads: usize) {
        let armed = self.armed();
        drop(
            self.changed
                .wait_while(armed, |armed| *armed != threads && self.state() == RUNNING)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Again,
    Wait,
    Close,
    Handoff,
}

pub(crate) struct ListenerFd<S: Service> {
    pub(crate) service: Arc<S>,
    pub(crate) timeouts: Timeouts,
    pub(crate) tls: Option<Arc<dyn TlsRouter>>,
    pub(crate) fd: OwnedFd,
    pub(crate) shared: bool,
}

pub(crate) struct Reactor<S: Service> {
    pub(crate) config: Config,
    pub(crate) driver: Driver,
    pub(crate) slab: Slab<S>,
    pub(crate) wheel: Wheel,
    pub(crate) pool: IdlePool,
    pub(crate) buffers: BufferPool,
    pub(crate) pipes: Option<Pipes>,
    pub(crate) services: Vec<(Shared<S>, Timeouts)>,
    pub(crate) tls: Vec<Option<Arc<dyn TlsRouter>>>,
    pub(crate) response: Heads,
    pub(crate) expired: Vec<u32>,
    pub(crate) clock: Clock,
    pub(crate) now: Instant,
    pub(crate) moment: Moment,
    pub(crate) live: usize,
    pub(crate) control: Arc<Control>,
    pub(crate) drain_deadline: Option<Instant>,
    pub(crate) wake: Waker,
    pub(crate) listeners: Vec<Token>,
    pub(crate) listening: Vec<bool>,
    pub(crate) armed: bool,
    pub(crate) spare: Spare,
    pub(crate) schedule: Option<Schedule<S::Admission>>,
}

impl<S: Service> Reactor<S> {
    pub(crate) fn new(
        config: &Config,
        listeners: Vec<ListenerFd<S>>,
        (wake, control): (Waker, Arc<Control>),
        plan: Option<Plan<S::Admission>>,
    ) -> io::Result<Self> {
        let driver = Driver::new(config)?;
        let pipes = config
            .splice
            .filter(|_| driver.splices())
            .map(Pipes::new)
            .transpose()?;
        let clock = Clock::start();
        let now = Instant::now();
        let shared = listeners.iter().map(|listener| listener.shared).collect();
        let mut reactor = Self {
            config: config.clone(),
            driver,
            slab: Slab::new(),
            wheel: Wheel::new(config.tick),
            pool: IdlePool::new(config.max_idle_upstreams, config.max_idle_per_target),
            buffers: BufferPool::default(),
            pipes,
            services: Vec::with_capacity(listeners.len()),
            tls: Vec::with_capacity(listeners.len()),
            response: Heads::new(),
            expired: Vec::new(),
            clock,
            now,
            moment: clock.moment(now),
            live: 0,
            control,
            drain_deadline: None,
            wake: wake.clone(),
            listeners: Vec::with_capacity(listeners.len()),
            listening: Vec::with_capacity(listeners.len()),
            armed: false,
            spare: Spare::reserve()?,
            schedule: plan.map(|plan| Schedule::new(plan, shared)),
        };
        reactor.watch_wake(wake)?;
        reactor.watch_inbox()?;
        for (index, listener) in listeners.into_iter().enumerate() {
            reactor.add_listener(index, listener)?;
        }
        reactor.set_armed(reactor.control.accepting())?;
        Ok(reactor)
    }

    fn watch_wake(&mut self, wake: Waker) -> io::Result<()> {
        let token = self.slab.insert(Body::Wake(wake));
        let Some(Body::Wake(wake)) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        self.driver.watch_wake(token, wake.fd())
    }

    fn add_listener(&mut self, index: usize, listener: ListenerFd<S>) -> io::Result<()> {
        sys::prepare_listener(&listener.fd)?;
        self.services
            .push((Rc::new(listener.service), listener.timeouts));
        self.tls.push(listener.tls);
        let token = self.slab.insert(Body::Listener {
            fd: listener.fd,
            index,
        });
        self.listeners.push(token);
        self.listening.push(false);
        let Some(Body::Listener { fd, .. }) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        self.driver.add_listener(token, fd)
    }

    pub(crate) fn set_armed(&mut self, armed: bool) -> io::Result<()> {
        if armed != self.armed {
            self.armed = armed;
            if armed {
                self.control.arm();
            } else {
                self.control.disarm();
            }
        }
        self.sync_listening()
    }

    fn serves(&self, listener: usize) -> bool {
        self.armed
            && self
                .schedule
                .as_ref()
                .is_none_or(|schedule| schedule.serves(listener))
    }

    fn sync_listening(&mut self) -> io::Result<()> {
        for (position, token) in self.listeners.iter().enumerate() {
            let serves = self.serves(position);
            if serves == self.listening[position] {
                continue;
            }
            let Some(Body::Listener { fd, .. }) = self.slab.body(*token) else {
                continue;
            };
            self.driver.set_listening(*token, fd, serves)?;
            self.listening[position] = serves;
        }
        Ok(())
    }

    pub(crate) fn run(mut self) -> io::Result<()> {
        let mut events: Vec<Polled> = Vec::with_capacity(self.config.events);
        loop {
            events.clear();
            let timeout = self.timeout();
            self.driver.wait(timeout, &mut events)?;
            self.now = Instant::now();
            self.moment = self.clock.moment(self.now);
            self.begin_batch(events.len());
            for event in events.drain(..) {
                self.dispatch(event);
            }
            self.serve_ready();
            self.fire_timers();
            if self.finished() {
                return Ok(());
            }
            self.account_busy();
        }
    }

    fn timeout(&self) -> Option<Duration> {
        if self.backlogged() {
            return Some(Duration::ZERO);
        }
        let wheel = self.wheel.next_wake(self.moment);
        let drain = self
            .drain_deadline
            .map(|at| at.saturating_duration_since(self.now));
        let wait = [wheel, drain].into_iter().flatten().min()?;
        Some(wait.max(Duration::from_millis(1)))
    }

    fn finished(&mut self) -> bool {
        if self.control.state() == STOPPING {
            self.abort_all();
        }
        if self.drain_deadline.is_some_and(|at| self.now >= at) {
            self.abort_all();
        }
        self.drain_deadline.is_some() && self.live == 0
    }

    fn dispatch(&mut self, event: Polled) {
        match event {
            Polled::Ready(token, flags) => self.ready(token, flags),
            Polled::Accepted(token, fd, peer) => self.accepted(token, fd, peer),
        }
    }

    fn ready(&mut self, token: Token, flags: EventFlags) {
        match self.slab.body(token) {
            Some(Body::Client(_)) => self.client_ready(token, flags),
            Some(Body::Upstream(_)) => self.upstream_event(token, flags),
            Some(Body::Listener { index, .. }) => {
                let index = *index;
                self.accept(token, index);
            }
            Some(Body::Wake(_)) => self.woken(token),
            Some(Body::Inbox(_)) => self.arrived(token),
            Some(Body::Free | Body::Busy) | None => {}
        }
    }

    fn fire_timers(&mut self) {
        let mut expired = std::mem::take(&mut self.expired);
        self.wheel.advance(self.moment, &mut expired);
        for index in expired.drain(..) {
            if let Some(token) = self.slab.live(index) {
                self.expire(token);
            }
        }
        self.expired = expired;
    }

    fn woken(&mut self, token: Token) {
        self.wake.settle();
        self.driver.wake_consumed(token);
        match self.control.state() {
            DRAINING | STOPPING => self.begin_drain(),
            _ if self.set_armed(self.control.accepting()).is_err() => self.control.raise(STOPPING),
            _ => {}
        }
        self.cancel_requested();
    }
}
