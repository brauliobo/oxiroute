use std::{
    io,
    os::fd::OwnedFd,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
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
    pub(crate) state: AtomicU8,
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
    pub(crate) now: Instant,
    pub(crate) live: usize,
    pub(crate) control: Arc<Control>,
    pub(crate) drain_deadline: Option<Instant>,
    pub(crate) wake: OwnedFd,
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
        let now = Instant::now();
        let mut reactor = Self {
            config: config.clone(),
            epoll,
            slab: Slab::new(capacity),
            wheel: Wheel::new(now, config.tick, capacity),
            pool: IdlePool::new(config.max_idle_upstreams, config.max_idle_per_target),
            spare: Vec::new(),
            services: Vec::with_capacity(listeners.len()),
            response: Box::new(ResponseHead::new()),
            expired: Vec::with_capacity(capacity),
            now,
            live: 0,
            control,
            drain_deadline: None,
            wake: wake.try_clone()?,
        };
        reactor.watch_wake(wake)?;
        for (index, listener) in listeners.into_iter().enumerate() {
            reactor.watch_listener(index, listener)?;
        }
        Ok(reactor)
    }

    fn watch_wake(&mut self, wake: OwnedFd) -> io::Result<()> {
        let token = self.slab.insert(Body::Wake(wake)).ok_or(Errno::NOSPC)?;
        let Some(Body::Wake(fd)) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        sys::watch(&self.epoll, fd, token.to_u64(), EventFlags::IN)
    }

    fn watch_listener(&mut self, index: usize, listener: ListenerFd<S>) -> io::Result<()> {
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
        let Some(Body::Listener { fd, .. }) = self.slab.body(token) else {
            unreachable!("just inserted")
        };
        sys::watch(
            &self.epoll,
            fd,
            token.to_u64(),
            EventFlags::IN | EventFlags::EXCLUSIVE,
        )
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
        let wheel = self.wheel.next_wake(self.now);
        let drain = self
            .drain_deadline
            .map(|at| at.saturating_duration_since(self.now));
        let wait = [wheel, drain].into_iter().flatten().min()?;
        Some(timespec(wait.max(Duration::from_millis(1))))
    }

    fn finished(&mut self) -> bool {
        if self.control.state.load(Ordering::Relaxed) == STOPPING {
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
        self.wheel.advance(self.now, &mut expired);
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
        match self.control.state.load(Ordering::Relaxed) {
            DRAINING | STOPPING => self.begin_drain(),
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
