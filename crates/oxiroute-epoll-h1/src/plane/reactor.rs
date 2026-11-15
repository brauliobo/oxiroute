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

use oxiroute_h1_head::ResponseHead;
use rustix::{
    buffer::spare_capacity,
    event::{
        Timespec,
        epoll::{self, Event, EventFlags},
    },
    io::Errno,
};

use super::{
    client::{Buffers, Shared},
    clock::{Clock, Moment},
    config::{Config, Timeouts},
    service::Service,
    slab::{Body, Slab, Token},
    sys,
    upstream::IdlePool,
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
    pub(crate) fd: OwnedFd,
}

pub(crate) struct Reactor<S: Service> {
    pub(crate) config: Config,
    pub(crate) epoll: OwnedFd,
    pub(crate) slab: Slab<S>,
    pub(crate) wheel: Wheel,
    pub(crate) pool: IdlePool,
    pub(crate) spare: Vec<Buffers>,
    pub(crate) services: Vec<(Shared<S>, Timeouts)>,
    pub(crate) response: Box<ResponseHead>,
    pub(crate) expired: Vec<u32>,
    pub(crate) clock: Clock,
    pub(crate) now: Instant,
    pub(crate) moment: Moment,
    pub(crate) live: usize,
    pub(crate) control: Arc<Control>,
    pub(crate) drain_deadline: Option<Instant>,
    pub(crate) wake: OwnedFd,
    pub(crate) listeners: Vec<Token>,
    pub(crate) armed: bool,
}

impl<S: Service> Reactor<S> {
    pub(crate) fn new(
        config: &Config,
        listeners: Vec<ListenerFd<S>>,
        wake: OwnedFd,
        control: Arc<Control>,
    ) -> io::Result<Self> {
        let epoll = epoll::create(epoll::CreateFlags::CLOEXEC)?;
        let capacity = config.slab_capacity(listeners.len());
        let clock = Clock::start();
        let now = Instant::now();
        let mut reactor = Self {
            config: config.clone(),
            epoll,
            slab: Slab::new(capacity),
            wheel: Wheel::new(config.tick, capacity),
            pool: IdlePool::new(config.max_idle_upstreams, config.max_idle_per_target),
            spare: Vec::new(),
            services: Vec::with_capacity(listeners.len()),
            response: Box::new(ResponseHead::new()),
            expired: Vec::with_capacity(capacity),
            clock,
            now,
            moment: clock.moment(now),
            live: 0,
            control,
            drain_deadline: None,
            wake: wake.try_clone()?,
            listeners: Vec::with_capacity(listeners.len()),
            armed: false,
        };
        reactor.watch_wake(wake)?;
        for (index, listener) in listeners.into_iter().enumerate() {
            reactor.add_listener(index, listener)?;
        }
        reactor.set_armed(reactor.control.accepting())?;
        Ok(reactor)
    }

    fn watch_wake(&mut self, wake: OwnedFd) -> io::Result<()> {
        let token = self.slab.insert(Body::Wake(wake)).ok_or(Errno::NOSPC)?;
        let Some(Body::Wake(fd)) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        sys::watch(&self.epoll, fd, token.to_u64(), EventFlags::IN)
    }

    fn add_listener(&mut self, index: usize, listener: ListenerFd<S>) -> io::Result<()> {
        sys::prepare_listener(&listener.fd)?;
        self.services
            .push((Rc::new(listener.service), listener.timeouts));
        let token = self
            .slab
            .insert(Body::Listener {
                fd: listener.fd,
                index,
            })
            .ok_or(Errno::NOSPC)?;
        self.listeners.push(token);
        Ok(())
    }

    pub(crate) fn set_armed(&mut self, armed: bool) -> io::Result<()> {
        if armed == self.armed {
            return Ok(());
        }
        for token in &self.listeners {
            let Some(Body::Listener { fd, .. }) = self.slab.body(*token) else {
                continue;
            };
            if armed {
                sys::watch(
                    &self.epoll,
                    fd,
                    token.to_u64(),
                    EventFlags::IN | EventFlags::EXCLUSIVE,
                )?;
            } else {
                sys::unwatch(&self.epoll, fd);
            }
        }
        self.armed = armed;
        if armed {
            self.control.arm();
        } else {
            self.control.disarm();
        }
        Ok(())
    }

    pub(crate) fn run(mut self) -> io::Result<()> {
        let mut events: Vec<Event> = Vec::with_capacity(self.config.events);
        loop {
            events.clear();
            let timeout = self.timeout();
            match epoll::wait(&self.epoll, spare_capacity(&mut events), timeout.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
            self.now = Instant::now();
            self.moment = self.clock.moment(self.now);
            for event in &events {
                self.dispatch(Token::from_u64(event.data.u64()), event.flags);
            }
            self.fire_timers();
            if self.finished() {
                return Ok(());
            }
        }
    }

    fn timeout(&self) -> Option<Timespec> {
        let wheel = self.wheel.next_wake(self.moment);
        let drain = self
            .drain_deadline
            .map(|at| at.saturating_duration_since(self.now));
        let wait = [wheel, drain].into_iter().flatten().min()?;
        Some(timespec(wait.max(Duration::from_millis(1))))
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

    fn dispatch(&mut self, token: Token, flags: EventFlags) {
        match self.slab.body(token) {
            Some(Body::Client(_)) => self.client_event(token, Some(flags)),
            Some(Body::Upstream(_)) => self.upstream_event(token, flags),
            Some(Body::Listener { index, .. }) => {
                let index = *index;
                self.accept(token, index);
            }
            Some(Body::Wake(_)) => self.woken(),
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

    fn woken(&mut self) {
        let mut counter = [0_u8; 8];
        let _ = rustix::io::read(&self.wake, &mut counter);
        match self.control.state() {
            DRAINING | STOPPING => self.begin_drain(),
            _ if self.set_armed(self.control.accepting()).is_err() => self.control.raise(STOPPING),
            _ => {}
        }
    }
}

fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(duration.subsec_nanos()),
    }
}
