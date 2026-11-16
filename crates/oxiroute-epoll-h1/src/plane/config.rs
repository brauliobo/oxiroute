use std::time::Duration;

use super::schedule::Scheduling;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    Edge,
    Level,
}

/// How an engine thread waits for and performs socket I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Driver {
    /// Readiness notification (`epoll`) and non-blocking system calls.
    Epoll,
    /// Completion-based `io_uring` with owned buffers; [`Config::uring`] tunes it. It serves plain HTTP with the
    /// kernel's spread of connections: the engine refuses TLS termination and per-worker scheduling for it, and
    /// never splices.
    IoUring,
}

impl Driver {
    pub(crate) const fn thread_prefix(self) -> &'static str {
        match self {
            Self::Epoll => "epoll",
            Self::IoUring => "uring",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Splice {
    pub threshold: u64,
    pub pipes: usize,
    pub pipe_bytes: usize,
}

impl Default for Splice {
    fn default() -> Self {
        Self {
            threshold: 16 * 1024,
            pipes: 64,
            pipe_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub threads: usize,
    pub max_idle_upstreams: usize,
    pub max_idle_per_target: usize,
    pub head_limit: usize,
    pub request_buffer: usize,
    pub response_buffer: usize,
    pub events: usize,
    pub starts_per_pass: usize,
    pub tick: Duration,
    pub drain: Duration,
    pub linger: Duration,
    pub max_attempts: u8,
    pub trigger: Trigger,
    pub driver: Driver,
    pub uring: oxiroute_uring::Config,
    pub scheduling: Option<Scheduling>,
    pub splice: Option<Splice>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threads: 1,
            max_idle_upstreams: 4_096,
            max_idle_per_target: 1_024,
            head_limit: 8 * 1024,
            request_buffer: 16 * 1024,
            response_buffer: 16 * 1024,
            events: 256,
            starts_per_pass: 8,
            tick: Duration::from_millis(10),
            drain: Duration::from_secs(30),
            linger: Duration::from_secs(1),
            max_attempts: 4,
            trigger: Trigger::Edge,
            driver: Driver::Epoll,
            uring: oxiroute_uring::Config::default(),
            scheduling: None,
            splice: Some(Splice::default()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub first_request: Option<Duration>,
    pub keepalive: Option<Duration>,
    pub request: Option<Duration>,
    pub write: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            first_request: Some(Duration::from_mins(1)),
            keepalive: Some(Duration::from_mins(1)),
            request: Some(Duration::from_mins(1)),
            write: Some(Duration::from_mins(1)),
        }
    }
}
