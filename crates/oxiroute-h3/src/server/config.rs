//! Settings of a connection.
use std::time::Duration;

/// Limits of an HTTP/3 server connection.
#[derive(Clone, Debug)]
pub struct Config {
    /// `SETTINGS_MAX_FIELD_SECTION_SIZE` announced to the peer, and the limit on the decoded size of a request head
    /// (name and value lengths summed over its fields). The encoded `HEADERS` frame may be at most
    /// [`HEADERS_FRAME_FACTOR`] times this; a larger one resets the stream with `H3_EXCESSIVE_LOAD`.
    pub max_field_section_size: u64,
    /// Request streams the peer may reset before the response was finished within [`Config::reset_flood_window`]
    /// before the connection is closed with `H3_EXCESSIVE_LOAD` (the "rapid reset" pattern).
    pub reset_flood_limit: u32,
    /// Length of the window of [`Config::reset_flood_limit`].
    pub reset_flood_window: Duration,
}

/// The encoded `HEADERS` frame may be this many times `max_field_section_size` (Huffman coding makes the decoded
/// size up to 8/5 of the encoded one, and literals are not compressed at all).
pub const HEADERS_FRAME_FACTOR: u64 = 4;

impl Default for Config {
    /// 16 KiB field sections; 1,000 resets within 10 seconds (a browser leaving a page cancels a few hundred
    /// requests at once, an attacker needs thousands per second).
    fn default() -> Self {
        Self {
            max_field_section_size: 16 * 1024,
            reset_flood_limit: 1000,
            reset_flood_window: Duration::from_secs(10),
        }
    }
}

impl Config {
    /// The largest `HEADERS` frame payload accepted.
    pub(crate) fn max_headers_frame(&self) -> u64 {
        self.max_field_section_size
            .saturating_mul(HEADERS_FRAME_FACTOR)
    }
}
