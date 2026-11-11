//! Sans-IO connection core: frame parsing, stream state machine, flow control, abuse limits and the output
//! queue. The async driver in [`crate::server`] feeds it bytes and drains [`Output`]; stream handles call the
//! poll-style methods in `api` under the connection mutex.
//!
//! Differences from the `h2` crate, all deliberate:
//! * HPACK failures are `COMPRESSION_ERROR` (RFC 9113 4.3); `h2` sends `PROTOCOL_ERROR`.
//! * Unknown pseudo-headers and pseudo-headers in trailers are a stream error (RFC 8.3); `h2` closes the
//!   connection.
//! * The first frame after the preface must be `SETTINGS` (RFC 3.4); `h2` does not check.
//! * Frames of a locally reset stream are ignored for as long as its id is among the last `reset_stream_max`
//!   resets (no timers), where `h2` also expires them after a second.
//! * Wrong frame sizes (`SETTINGS`, `PING`, `RST_STREAM`, `WINDOW_UPDATE`) are `FRAME_SIZE_ERROR`, an
//!   `INITIAL_WINDOW_SIZE` above 2^31-1 is `FLOW_CONTROL_ERROR` (RFC 6.5.2); `h2` sends `PROTOCOL_ERROR`.
//! * A `PRIORITY` of the wrong size and a zero `WINDOW_UPDATE` on a stream are stream errors (RFC 6.3, 6.9);
//!   `h2` closes the connection. `PRIORITY` frames are counted (`max_ignored_frames`), `h2` ignores them.
//! * An uppercase field name is a stream error (RFC 8.2.1) and `CONNECT` without `:authority` is malformed (RFC
//!   8.5); `h2` closes the connection for the first and accepts the second.
//! * `HEADERS` or `DATA` on a half-closed (remote) stream are a stream error `STREAM_CLOSED` (RFC 5.1); `h2`
//!   closes the connection.
//! * No 4x "way too large" abuse multiplier on the decoded header list: every request over the limit is
//!   answered 431 and counted against `max_local_error_reset_streams`.
//! * Peer resets of accepted streams are limited (`max_peer_reset_streams`), `h2` limits only resets of
//!   streams not yet accepted.
//! * The `RST_STREAM`s queued before a connection error are still flushed ahead of the `GOAWAY`, and no
//!   `GOAWAY` answers the peer's own.
//! * Our advertised `SETTINGS` apply from the moment they are sent, `h2` applies them when acknowledged.
use std::collections::VecDeque;
use std::task::Waker;

use bytes::Bytes;

use crate::error::{Error, Initiator, Reason};
use crate::frame::{self, DEFAULT_MAX_FRAME_SIZE, DEFAULT_WINDOW};
use crate::hpack::{Decoder, Encoder};
use crate::server::Config;

mod api;
mod input;
mod output;
mod parts;
mod request;
mod response;
mod streams;
mod validate;
mod view;

pub use output::{COPY_LIMIT, Output, empty_slices};
pub(crate) use parts::FieldCache;
use streams::Streams;
pub use view::{Fields, RequestView};

const SHUTDOWN_PING: [u8; 8] = [0x0b, 0x7b, 0xa2, 0xf0, 0x8b, 0x9b, 0xfe, 0x54];
const OVERHEAD_THRESHOLD: usize = 256;

/// A failure while handling a frame.
pub(crate) enum Fail {
    Conn(Reason),
    Stream(u32, Reason),
}

type Res = Result<(), Fail>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shutdown {
    None,
    /// First GOAWAY (last stream 2^31-1) and the PING are out; waiting for the PING ack.
    Draining,
    /// Final GOAWAY sent: streams above `goaway_last` are ignored.
    GoAwaySent,
}

/// A request waiting for `accept`; `view` is `None` once the peer reset the stream.
pub(crate) struct Pending {
    id: u32,
    idx: u32,
    view: Option<RequestView>,
    /// The peer's reset of this stream counts against `max_pending_accept_reset_streams` until it is dequeued.
    counted: bool,
}

#[derive(Clone, Copy)]
enum Target {
    New,
    Refuse,
    Trailers(u32),
    Ignore,
    Reject(Reason),
}

struct Cont {
    id: u32,
    end_stream: bool,
    target: Target,
    block: Vec<u8>,
    frames: usize,
}

/// The HTTP/2 server connection state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct Connection {
    cfg: Config,
    out: Output,
    streams: Streams,
    dec: Decoder,
    /// The last `:authority` that passed validation.
    authority: Vec<u8>,
    enc: Encoder,
    scratch: Vec<u8>,
    send_window: i64,
    send_reserved: i64,
    peer_initial_window: i64,
    peer_max_frame: usize,
    recv_window: i64,
    conn_unreleased: u32,
    highest_id: u32,
    goaway_last: Option<u32>,
    recent_resets: VecDeque<u32>,
    accept: VecDeque<Pending>,
    live_pending: usize,
    cont: Option<Cont>,
    got_settings: bool,
    acks_pending: u32,
    shutdown: Shutdown,
    peer_goaway: bool,
    closed: Option<Error>,
    report: bool,
    open_streams: usize,
    pending_resets: usize,
    local_error_resets: usize,
    peer_resets: usize,
    ctl_frames: usize,
    ignored: usize,
    empty_data: usize,
    data_budget: usize,
    driver: Option<Waker>,
    /// The driver wants a wake-up on the next push to the output queue (cleared by the wake-up).
    armed: bool,
    cap_waiters: VecDeque<u32>,
    need: usize,
}

impl Connection {
    /// A connection whose initial SETTINGS (and connection `WINDOW_UPDATE`) are queued in the output.
    #[must_use]
    pub fn new(cfg: Config) -> Self {
        let mut c = Self {
            cfg,
            out: Output::default(),
            streams: Streams::default(),
            dec: Decoder::new(cfg.header_table_size as usize),
            authority: Vec::new(),
            enc: Encoder::new(4096),
            scratch: Vec::new(),
            send_window: i64::from(DEFAULT_WINDOW),
            send_reserved: 0,
            peer_initial_window: i64::from(DEFAULT_WINDOW),
            peer_max_frame: DEFAULT_MAX_FRAME_SIZE as usize,
            recv_window: i64::from(cfg.initial_connection_window.max(DEFAULT_WINDOW)),
            conn_unreleased: 0,
            highest_id: 0,
            goaway_last: None,
            recent_resets: VecDeque::new(),
            accept: VecDeque::new(),
            live_pending: 0,
            cont: None,
            got_settings: false,
            acks_pending: 1,
            shutdown: Shutdown::None,
            peer_goaway: false,
            closed: None,
            report: false,
            open_streams: 0,
            pending_resets: 0,
            local_error_resets: 0,
            peer_resets: 0,
            ctl_frames: 0,
            ignored: 0,
            empty_data: 0,
            data_budget: cfg.data_frame_budget,
            driver: None,
            armed: false,
            cap_waiters: VecDeque::new(),
            need: 0,
        };
        c.queue_initial_settings();
        c
    }

    fn queue_initial_settings(&mut self) {
        let cfg = self.cfg;
        let mut entries = Vec::with_capacity(6);
        if cfg.header_table_size != 4096 {
            entries.push((1, cfg.header_table_size));
        }
        entries.push((3, cfg.max_concurrent_streams));
        if cfg.initial_stream_window != DEFAULT_WINDOW {
            entries.push((4, cfg.initial_stream_window));
        }
        if cfg.max_frame_size != DEFAULT_MAX_FRAME_SIZE {
            entries.push((5, cfg.max_frame_size));
        }
        entries.push((6, cfg.max_header_list_size));
        frame::settings_frame(&mut self.out.tail, &entries);
        if cfg.initial_connection_window > DEFAULT_WINDOW {
            frame::window_update(
                &mut self.out.tail,
                0,
                cfg.initial_connection_window - DEFAULT_WINDOW,
            );
        }
    }

    /// Moves the queued frames into `into` (which must be empty) and starts a new control-frame budget.
    pub fn take_output(&mut self, into: &mut Output) {
        std::mem::swap(&mut self.out, into);
        self.ctl_frames = 0;
    }

    #[must_use]
    pub fn has_output(&self) -> bool {
        !self.out.is_empty()
    }

    /// Total octets needed in the input buffer to complete the next frame (0 while its header is incomplete).
    #[must_use]
    pub fn needed(&self) -> usize {
        self.need
    }

    /// The connection is over: an error closed it or the peer went away.
    #[must_use]
    pub fn closed(&self) -> Option<&Error> {
        self.closed.as_ref()
    }

    /// The error `accept` still has to report, once.
    pub fn take_report(&mut self) -> Option<Error> {
        if std::mem::take(&mut self.report) {
            self.closed.clone()
        } else {
            None
        }
    }

    /// No stream is open or waiting and the connection is draining after a `GOAWAY` by either side.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        (self.shutdown == Shutdown::GoAwaySent || self.peer_goaway)
            && self.open_streams == 0
            && self.live_pending == 0
    }

    /// Time the peer has to acknowledge our SETTINGS (zero: no limit).
    #[must_use]
    pub fn settings_ack_timeout(&self) -> std::time::Duration {
        self.cfg.settings_ack_timeout
    }

    #[must_use]
    pub fn awaiting_settings_ack(&self) -> bool {
        self.acks_pending > 0 && self.closed.is_none()
    }

    /// The driver is about to poll (or return control): it wants one wake-up for new output or a drained state.
    pub(crate) fn arm_driver(&mut self, waker: &Waker) {
        if !self.driver.as_ref().is_some_and(|w| w.will_wake(waker)) {
            self.driver = Some(waker.clone());
        }
        self.armed = true;
    }

    /// The driver returns a request and polls again right away: no wake-up needed for new output meanwhile.
    pub(crate) fn disarm_driver(&mut self) {
        self.armed = false;
    }

    /// Wakes the driver once per arming.
    fn wake_driver(&mut self) {
        if std::mem::take(&mut self.armed)
            && let Some(w) = &self.driver
        {
            w.wake_by_ref();
        }
    }

    /// Runs `f` (a stream handle operation) and wakes the driver if there is output to write.
    fn kick<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let r = f(self);
        if !self.out.is_empty() {
            self.wake_driver();
        }
        r
    }

    // ---- errors and closing ----

    /// Ends the connection with `GOAWAY(reason)` for a protocol violation of the peer.
    pub fn conn_error(&mut self, reason: Reason) {
        if self.closed.is_none() {
            frame::go_away(&mut self.out.tail, self.highest_id, reason.code());
            self.close(Error::go_away(reason, Initiator::Library), true);
        }
    }

    /// `abrupt_shutdown`: `GOAWAY(reason)` for the application's reasons; `accept` ends without an error.
    pub fn abrupt_shutdown(&mut self, reason: Reason) {
        if self.closed.is_none() {
            frame::go_away(&mut self.out.tail, self.highest_id, reason.code());
            self.close(Error::go_away(reason, Initiator::User), false);
        }
    }

    /// The transport failed or ended.
    pub fn transport_closed(&mut self, error: Option<Error>) {
        if self.closed.is_none() {
            let report = error.is_some();
            self.close(error.unwrap_or_else(Error::closed), report);
        }
    }

    fn close(&mut self, error: Error, report: bool) {
        self.closed = Some(error);
        self.report = report;
        self.accept.clear();
        self.live_pending = 0;
        for s in self.streams.iter_mut() {
            s.wake_all();
        }
        self.wake_cap_waiters();
    }

    /// Records the stream's id so that frames already in flight for it are ignored.
    fn remember_reset(&mut self, id: u32) {
        if self.cfg.reset_stream_max == 0 {
            return;
        }
        if self.recent_resets.len() >= self.cfg.reset_stream_max {
            self.recent_resets.pop_front();
        }
        self.recent_resets.push_back(id);
    }

    /// A reset by either side: the stream stops counting as open, its reservations and buffered body are
    /// released and its waiters woken. Streams the application never saw are dropped.
    fn reset_stream(&mut self, idx: u32, reason: Reason, by: Initiator) {
        let s = self.streams.slot(idx);
        if s.reset.is_some() {
            return;
        }
        s.reset = Some((reason, by));
        let (id, accepted) = (s.id, s.accepted);
        self.release_reservation(idx);
        self.discard_body(idx);
        self.update_open(idx);
        self.streams.slot(idx).wake_all();
        if !accepted {
            self.kill_pending(id, by == Initiator::Remote);
            self.streams.remove(id);
        }
        if by != Initiator::Remote {
            self.remember_reset(id);
        }
    }

    fn kill_pending(&mut self, id: u32, counted: bool) {
        if let Some(p) = self.accept.iter_mut().find(|p| p.id == id) {
            if p.view.take().is_some() {
                self.live_pending -= 1;
            }
            p.counted = counted;
        }
    }

    /// Stream error detected on a peer frame: `RST_STREAM`, counted against the local error budget.
    fn stream_error(&mut self, id: u32, reason: Reason) {
        frame::rst_stream(&mut self.out.tail, id, reason.code());
        match self.streams.find(id) {
            Some(idx) => self.reset_stream(idx, reason, Initiator::Library),
            None => self.remember_reset(id),
        }
        self.count_local_error();
    }

    /// Counts an error answer sent for a peer protocol violation against `max_local_error_reset_streams`.
    fn count_local_error(&mut self) {
        self.local_error_resets += 1;
        if self.local_error_resets > self.cfg.max_local_error_reset_streams {
            self.conn_error(Reason::ENHANCE_YOUR_CALM);
        } else {
            self.control_frame();
        }
    }

    /// Accounts a control frame queued because of something the peer sent.
    fn control_frame(&mut self) {
        self.ctl_frames += 1;
        if self.ctl_frames > self.cfg.max_pending_control_frames {
            self.conn_error(Reason::ENHANCE_YOUR_CALM);
        }
    }

    /// A stream both sides are done with stops counting against `max_concurrent_streams`.
    fn update_open(&mut self, idx: u32) {
        let s = self.streams.slot(idx);
        if !s.open || !s.is_closed() {
            return;
        }
        s.open = false;
        if s.reset.is_none() {
            self.peer_resets = self.peer_resets.saturating_sub(1);
            self.ignored = self.ignored.saturating_sub(1);
        }
        self.open_streams -= 1;
        if self.is_drained() {
            self.wake_driver();
        }
    }

    // ---- flow control bookkeeping ----

    /// Connection receive credit given back by the application (or automatically): batched `WINDOW_UPDATE`.
    fn release_conn(&mut self, n: u32) {
        self.conn_unreleased += n;
        let target = self.cfg.initial_connection_window.max(DEFAULT_WINDOW);
        if self.conn_unreleased >= target / 2 {
            frame::window_update(&mut self.out.tail, 0, self.conn_unreleased);
            self.recv_window += i64::from(self.conn_unreleased);
            self.conn_unreleased = 0;
        }
    }

    fn release_reservation(&mut self, idx: u32) {
        let s = self.streams.slot(idx);
        let reserved = std::mem::take(&mut s.reserved);
        if reserved > 0 {
            self.send_reserved -= i64::from(reserved);
            self.wake_cap_waiters();
        }
    }

    /// Frees buffered request data nobody will read: connection credit and small-frame budget come back.
    fn discard_body(&mut self, idx: u32) {
        let Some(body) = self.streams.slot(idx).body.as_mut() else {
            return;
        };
        let chunks = std::mem::take(&mut body.chunks);
        for c in chunks {
            self.replenish(&c.data, c.budgeted);
            self.release_conn(c.data.len() as u32);
        }
    }

    fn replenish(&mut self, data: &Bytes, budgeted: bool) {
        if budgeted && data.len() < OVERHEAD_THRESHOLD {
            self.data_budget = (self.data_budget + OVERHEAD_THRESHOLD - data.len())
                .min(self.cfg.data_frame_budget);
        }
    }

    fn wake_cap_waiters(&mut self) {
        while let Some(id) = self.cap_waiters.pop_front() {
            if let Some(idx) = self.streams.find(id) {
                let s = self.streams.slot(idx);
                s.waiting_conn = false;
                if let Some(w) = s.wakers.send.take() {
                    w.wake();
                }
            }
        }
    }
}
