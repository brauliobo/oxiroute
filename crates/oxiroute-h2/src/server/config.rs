//! Connection limits and tuning knobs; `Config::default()` is the protocol profile, the proxy applies its own on top.
use std::time::Duration;

/// Settings advertised to the peer and abuse limits enforced on the connection.
///
/// The limits mirror the `h2` crate's server builder where one exists (same names, same defaults); the
/// remaining fields cover protections this crate adds.
///
/// Only the two receive windows are operator options (`h2_initial_stream_window` and `h2_initial_connection_window`
/// of a listener, 1 MiB and 8 MiB unless set). The proxy also sets `max_header_list_size` (64 KiB, or the forward
/// proxy's configured `max_header_bytes`) and `max_concurrent_streams` (100) and keeps every other field at its
/// default. The abuse limits are protocol-safety bounds with no deployment-dependent right value (each is the `h2`
/// crate's own default where one exists, which carried the same traffic before 6.4; the others are generous bounds
/// chosen with the regression tests of each protection).
///
/// The windows were fixed at the protocol's 65,535 until 6.4 so that a connection's buffered memory stayed bounded by
/// 64 KiB. That bound was bought with throughput: an upload cost one `WINDOW_UPDATE` per 32 KiB on the stream and
/// another on the connection (64 per MiB, each its own TLS record and write) and a stream could move at most 64 KiB
/// per round trip. Credit goes back to the peer only when the application has read the data, so what a connection
/// holds for an application that reads nothing is bounded by its connection window, however many streams carry it,
/// and by the stream window for each stream: the window is the memory bound, and it is now chosen per listener. A
/// value that needs to change for a deployment becomes a configuration option, never an environment variable.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// `SETTINGS_MAX_CONCURRENT_STREAMS`; a stream opened beyond it is refused (`REFUSED_STREAM`).
    pub max_concurrent_streams: u32,
    /// `SETTINGS_MAX_HEADER_LIST_SIZE`, advertised and enforced: a larger request is answered with 431 and reset.
    pub max_header_list_size: u32,
    /// Initial per-stream receive window (`SETTINGS_INITIAL_WINDOW_SIZE`).
    pub initial_stream_window: u32,
    /// Connection receive window; when above 65,535 a `WINDOW_UPDATE` on stream 0 follows the first `SETTINGS`.
    pub initial_connection_window: u32,
    /// `SETTINGS_MAX_FRAME_SIZE`: the largest frame payload accepted (`FRAME_SIZE_ERROR` above it).
    pub max_frame_size: u32,
    /// `SETTINGS_HEADER_TABLE_SIZE` of the request decoder.
    pub header_table_size: u32,
    /// Peer resets of streams the application has not accepted yet that are tolerated at a time
    /// (CVE-2023-44487); one more is `GOAWAY(ENHANCE_YOUR_CALM)`.
    pub max_pending_accept_reset_streams: usize,
    /// Streams this side resets because of a peer protocol error, over the connection's life (`h2`:
    /// `max_local_error_reset_streams`); one more is `GOAWAY(ENHANCE_YOUR_CALM)`.
    pub max_local_error_reset_streams: usize,
    /// Ids of locally reset streams remembered so that frames already in flight are ignored (`h2`:
    /// `reset_stream_max`; this crate has no timers, so the oldest id is forgotten instead of expiring).
    pub reset_stream_max: usize,
    /// Peer `RST_STREAM` frames for streams that were already accepted, beyond the number of streams completed
    /// normally, that are tolerated (rapid reset keeps the application busy with cancelled requests).
    pub max_peer_reset_streams: usize,
    /// CONTINUATION frames per header block; `None` derives it from `max_header_list_size` like `h2`.
    pub max_continuation_frames: Option<usize>,
    /// Total header block octets per request; `None` uses `max_header_list_size` (a compressed block is never
    /// larger than its decoded list, which counts 32 octets of overhead per field).
    pub max_header_block_bytes: Option<usize>,
    /// Control frames (`PING`, `SETTINGS ACK`, `RST_STREAM`, `WINDOW_UPDATE`) queued for a peer that does not read them.
    pub max_pending_control_frames: usize,
    /// PRIORITY and unknown frames tolerated; each completed request earns one back.
    pub max_ignored_frames: usize,
    /// Empty non-final `DATA` frames over the connection's life.
    pub max_empty_data_frames: usize,
    /// Memory-overhead budget in octets for small `DATA` frames the application has not read (`h2`:
    /// `data_frame_budget`; a frame below 256 octets costs `256 - len`, larger frames earn the excess back).
    pub data_frame_budget: usize,
    /// The peer must acknowledge our `SETTINGS` within this time (`GOAWAY(SETTINGS_TIMEOUT)`); zero disables it.
    pub settings_ack_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_concurrent_streams: 100,
            max_header_list_size: 64 * 1024,
            initial_stream_window: 65_535,
            initial_connection_window: 65_535,
            max_frame_size: 16_384,
            header_table_size: 4096,
            max_pending_accept_reset_streams: 20,
            max_local_error_reset_streams: 1024,
            reset_stream_max: 50,
            max_peer_reset_streams: 256,
            max_continuation_frames: None,
            max_header_block_bytes: None,
            max_pending_control_frames: 1000,
            max_ignored_frames: 10_000,
            max_empty_data_frames: 100,
            data_frame_budget: 25_600,
            settings_ack_timeout: Duration::from_secs(10),
        }
    }
}

// The builder spelling follows `h2::server::Builder`; names that differ from the field are aliases.
impl Config {
    pub fn max_concurrent_streams(&mut self, value: u32) -> &mut Self {
        self.max_concurrent_streams = value;
        self
    }

    pub fn max_header_list_size(&mut self, value: u32) -> &mut Self {
        self.max_header_list_size = value;
        self
    }

    /// Per-stream receive window.
    pub fn initial_window_size(&mut self, value: u32) -> &mut Self {
        self.initial_stream_window = value;
        self
    }

    pub fn initial_connection_window_size(&mut self, value: u32) -> &mut Self {
        self.initial_connection_window = value;
        self
    }

    pub fn max_frame_size(&mut self, value: u32) -> &mut Self {
        self.max_frame_size = value;
        self
    }

    pub fn header_table_size(&mut self, value: u32) -> &mut Self {
        self.header_table_size = value;
        self
    }

    pub fn max_pending_accept_reset_streams(&mut self, value: usize) -> &mut Self {
        self.max_pending_accept_reset_streams = value;
        self
    }

    pub fn max_local_error_reset_streams(&mut self, value: usize) -> &mut Self {
        self.max_local_error_reset_streams = value;
        self
    }

    pub fn reset_stream_max(&mut self, value: usize) -> &mut Self {
        self.reset_stream_max = value;
        self
    }

    pub fn max_peer_reset_streams(&mut self, value: usize) -> &mut Self {
        self.max_peer_reset_streams = value;
        self
    }

    pub fn max_continuation_frames(&mut self, value: usize) -> &mut Self {
        self.max_continuation_frames = Some(value);
        self
    }

    pub fn max_header_block_bytes(&mut self, value: usize) -> &mut Self {
        self.max_header_block_bytes = Some(value);
        self
    }

    pub fn max_pending_control_frames(&mut self, value: usize) -> &mut Self {
        self.max_pending_control_frames = value;
        self
    }

    pub fn max_ignored_frames(&mut self, value: usize) -> &mut Self {
        self.max_ignored_frames = value;
        self
    }

    pub fn max_empty_data_frames(&mut self, value: usize) -> &mut Self {
        self.max_empty_data_frames = value;
        self
    }

    pub fn data_frame_budget(&mut self, value: usize) -> &mut Self {
        self.data_frame_budget = value;
        self
    }

    pub fn settings_ack_timeout(&mut self, value: Duration) -> &mut Self {
        self.settings_ack_timeout = value;
        self
    }

    /// `max_continuation_frames` or the `h2` derivation: frames needed for a full list plus 25 percent, at least 5.
    #[must_use]
    pub fn continuation_frame_limit(&self) -> usize {
        self.max_continuation_frames.unwrap_or_else(|| {
            let min = (self.max_header_list_size as usize / self.max_frame_size as usize).max(1);
            (min + (min >> 2)).max(5)
        })
    }

    #[must_use]
    pub fn header_block_limit(&self) -> usize {
        self.max_header_block_bytes
            .unwrap_or(self.max_header_list_size as usize)
    }
}
