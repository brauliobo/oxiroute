//! The peer's control stream (RFC 9114 section 6.2.1, 7.2.4 to 7.2.7).
use bytes::{Buf, Bytes};

use crate::code::Code;
use crate::frame::settings::{self, PeerSettings};
use crate::frame::{self, HeaderReader};
use crate::varint;

/// `SETTINGS` payloads above this are refused: real ones are a few dozen bytes.
const MAX_SETTINGS_PAYLOAD: u64 = 4096;
/// `GOAWAY`, `MAX_PUSH_ID` and `CANCEL_PUSH` carry one variable-length integer.
const MAX_ID_PAYLOAD: u64 = 8;

enum State {
    Between,
    Payload { kind: u64, remaining: u64 },
    Skip(u64),
}

/// A violation: the connection is closed with the code.
pub type Violation = (Code, &'static str);

pub struct Control {
    header: HeaderReader,
    state: State,
    payload: Vec<u8>,
    settings: Option<PeerSettings>,
    goaway: Option<u64>,
    max_push_id: Option<u64>,
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    #[must_use]
    pub fn new() -> Self {
        Self {
            header: HeaderReader::default(),
            state: State::Between,
            payload: Vec::new(),
            settings: None,
            goaway: None,
            max_push_id: None,
        }
    }

    #[must_use]
    pub fn settings(&self) -> Option<PeerSettings> {
        self.settings
    }

    /// Processes bytes read from the stream. Returns whether the peer sent a `GOAWAY` while doing so.
    ///
    /// # Errors
    ///
    /// The connection violation, with the code to close it with.
    pub fn feed(&mut self, mut data: Bytes) -> Result<bool, Violation> {
        let mut goaway = false;
        while data.has_remaining() || matches!(self.state, State::Payload { remaining: 0, .. }) {
            match self.state {
                State::Between => {
                    let Some((kind, len)) = self.header.read(&mut data) else {
                        break;
                    };
                    self.begin(kind, len)?;
                }
                State::Skip(remaining) => {
                    let take = remaining.min(data.remaining() as u64);
                    data.advance(take as usize);
                    self.state = if take == remaining {
                        State::Between
                    } else {
                        State::Skip(remaining - take)
                    };
                }
                State::Payload { kind, remaining } => {
                    let take = remaining.min(data.remaining() as u64);
                    self.payload
                        .extend_from_slice(&data.split_to(take as usize));
                    if take < remaining {
                        self.state = State::Payload {
                            kind,
                            remaining: remaining - take,
                        };
                        continue;
                    }
                    self.state = State::Between;
                    goaway |= self.complete(kind)?;
                    self.payload.clear();
                }
            }
        }
        Ok(goaway)
    }

    fn begin(&mut self, kind: u64, len: u64) -> Result<(), Violation> {
        if self.settings.is_none() && kind != frame::SETTINGS {
            return Err((
                Code::H3_MISSING_SETTINGS,
                "the first control frame is not SETTINGS",
            ));
        }
        self.state = match kind {
            frame::SETTINGS if self.settings.is_some() => {
                return Err((Code::H3_FRAME_UNEXPECTED, "second SETTINGS"));
            }
            frame::SETTINGS if len > MAX_SETTINGS_PAYLOAD => {
                return Err((Code::H3_EXCESSIVE_LOAD, "SETTINGS frame too large"));
            }
            frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH if len > MAX_ID_PAYLOAD => {
                return Err((Code::H3_FRAME_ERROR, "frame payload too long"));
            }
            frame::SETTINGS | frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH => {
                State::Payload {
                    kind,
                    remaining: len,
                }
            }
            frame::DATA | frame::HEADERS | frame::PUSH_PROMISE => {
                return Err((
                    Code::H3_FRAME_UNEXPECTED,
                    "frame not allowed on the control stream",
                ));
            }
            kind if frame::is_reserved_h2(kind) => {
                return Err((Code::H3_FRAME_UNEXPECTED, "reserved frame type"));
            }
            _ => State::Skip(len),
        };
        Ok(())
    }

    /// Acts on a complete frame; whether it was a `GOAWAY`.
    fn complete(&mut self, kind: u64) -> Result<bool, Violation> {
        if kind == frame::SETTINGS {
            self.settings =
                Some(settings::parse(&self.payload).map_err(|code| (code, "invalid SETTINGS"))?);
            return Ok(false);
        }
        let mut payload = &self.payload[..];
        let id =
            varint::decode(&mut payload).ok_or((Code::H3_FRAME_ERROR, "truncated identifier"))?;
        if !payload.is_empty() {
            return Err((Code::H3_FRAME_ERROR, "frame longer than its identifier"));
        }
        match kind {
            frame::GOAWAY => {
                if self.goaway.is_some_and(|previous| id > previous) {
                    return Err((Code::H3_ID_ERROR, "GOAWAY identifier increased"));
                }
                self.goaway = Some(id);
                Ok(true)
            }
            frame::MAX_PUSH_ID => {
                if self.max_push_id.is_some_and(|previous| id < previous) {
                    return Err((Code::H3_ID_ERROR, "MAX_PUSH_ID decreased"));
                }
                self.max_push_id = Some(id);
                Ok(false)
            }
            _ => Err((
                Code::H3_ID_ERROR,
                "CANCEL_PUSH for a push that was never promised",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
