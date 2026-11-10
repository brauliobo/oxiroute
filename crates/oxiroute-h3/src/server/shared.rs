//! State shared by a connection and its request streams.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use atomic_waker::AtomicWaker;

use super::Config;
use crate::code::Code;
use crate::error::Error;

pub(crate) struct Shared {
    pub conn: quinn::Connection,
    pub config: Config,
    /// Request streams handed out and not yet dropped.
    active: AtomicUsize,
    /// A GOAWAY was sent or received: `accept` ends once no request is active.
    closing: AtomicBool,
    idle: AtomicWaker,
    resets: Mutex<ResetWindow>,
    /// The control stream of a dropped `Connection`: finishing it would be a closed critical stream.
    kept_control: Mutex<Option<quinn::SendStream>>,
}

/// Fixed window counter of peer resets.
struct ResetWindow {
    start: Instant,
    count: u32,
}

impl Shared {
    pub fn new(conn: quinn::Connection, config: Config) -> Self {
        Self {
            conn,
            config,
            active: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            idle: AtomicWaker::new(),
            resets: Mutex::new(ResetWindow {
                start: Instant::now(),
                count: 0,
            }),
            kept_control: Mutex::new(None),
        }
    }

    /// Closes the connection for a protocol violation of the peer.
    pub fn fail(&self, code: Code, reason: &'static str) -> Error {
        self.conn.close(code.varint(), reason.as_bytes());
        Error::Protocol { code, reason }
    }

    pub fn keep_control(&self, stream: quinn::SendStream) {
        *self
            .kept_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stream);
    }

    pub fn request_started(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_ended(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 && self.closing.load(Ordering::Acquire) {
            self.idle.wake();
        }
    }

    pub fn start_closing(&self) {
        self.closing.store(true, Ordering::Release);
    }

    /// Whether a GOAWAY was exchanged and every request is done; registers `waker` otherwise.
    pub fn is_drained(&self, waker: &std::task::Waker) -> bool {
        if !self.closing.load(Ordering::Acquire) {
            return false;
        }
        self.idle.register(waker);
        self.active.load(Ordering::Acquire) == 0
    }

    /// Records a request stream the peer reset (or stopped) before the response was finished, and closes the
    /// connection when there were too many within the window.
    pub fn peer_reset(&self) -> Option<Error> {
        let now = Instant::now();
        let mut window = self
            .resets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.duration_since(window.start) >= self.config.reset_flood_window {
            *window = ResetWindow {
                start: now,
                count: 0,
            };
        }
        window.count += 1;
        let exceeded = window.count > self.config.reset_flood_limit;
        drop(window);
        exceeded.then(|| {
            self.fail(
                Code::H3_EXCESSIVE_LOAD,
                "too many request streams reset by the peer",
            )
        })
    }
}
