//! Frame dispatch: parses buffered frames and applies them to the connection and stream state.
use bytes::{Buf, BytesMut};
use http::HeaderMap;

use super::request::{self, Block, NO_CONTENT_LENGTH};
use super::streams::{Chunk, RecvState};
use super::{
    Connection, Cont, Fail, OVERHEAD_THRESHOLD, Pending, Res, SHUTDOWN_PING, Shutdown, Target,
};
use crate::error::{Initiator, Reason};
use crate::frame::{self, HEAD_LEN, Head, MAX_WINDOW, Settings};

const PROTOCOL: Fail = Fail::Conn(Reason::PROTOCOL_ERROR);
const FRAME_SIZE: Fail = Fail::Conn(Reason::FRAME_SIZE_ERROR);
const FLOW_CONTROL: Fail = Fail::Conn(Reason::FLOW_CONTROL_ERROR);

impl Connection {
    /// Parses and applies every complete frame in `input` (after the client preface); a partial frame stays.
    pub fn feed(&mut self, input: &mut BytesMut) {
        self.need = 0;
        while self.closed.is_none() && input.len() >= HEAD_LEN {
            let head = Head::parse(input);
            if head.len > self.cfg.max_frame_size as usize {
                self.conn_error(Reason::FRAME_SIZE_ERROR);
                break;
            }
            if input.len() < HEAD_LEN + head.len {
                self.need = HEAD_LEN + head.len;
                break;
            }
            input.advance(HEAD_LEN);
            if let Err(fail) = self.frame(head, input) {
                match fail {
                    Fail::Conn(reason) => self.conn_error(reason),
                    Fail::Stream(id, reason) => self.stream_error(id, reason),
                }
            }
        }
    }

    fn frame(&mut self, head: Head, input: &mut BytesMut) -> Res {
        if let Some(c) = &self.cont {
            if head.kind != frame::CONTINUATION || head.stream != c.id {
                return Err(PROTOCOL);
            }
        }
        if !self.got_settings && head.kind != frame::SETTINGS {
            return Err(PROTOCOL);
        }
        if head.kind == frame::DATA {
            return self.on_data(head, input.split_to(head.len));
        }
        let result = {
            let payload = &input[..head.len];
            match head.kind {
                frame::HEADERS => self.on_headers(head, payload),
                frame::CONTINUATION => self.on_continuation(head, payload),
                frame::PRIORITY => self.on_priority(head, payload),
                frame::RST_STREAM => self.on_rst_stream(head, payload),
                frame::SETTINGS => self.on_settings(head, payload),
                frame::PUSH_PROMISE => Err(PROTOCOL),
                frame::PING => self.on_ping(head, payload),
                frame::GOAWAY => self.on_goaway(head),
                frame::WINDOW_UPDATE => self.on_window_update(head, payload),
                _ => self.ignore_frame(),
            }
        };
        input.advance(head.len);
        result
    }

    /// Cheap frames that need no answer share one budget; every completed request earns one back.
    fn ignore_frame(&mut self) -> Res {
        self.ignored += 1;
        if self.ignored > self.cfg.max_ignored_frames {
            return Err(Fail::Conn(Reason::ENHANCE_YOUR_CALM));
        }
        Ok(())
    }

    // ---- DATA ----

    fn on_data(&mut self, head: Head, mut payload: BytesMut) -> Res {
        if head.stream == 0 {
            return Err(PROTOCOL);
        }
        let total = head.len;
        if head.flags & frame::PADDED != 0 {
            let pad = usize::from(*payload.first().ok_or(PROTOCOL)?);
            if pad >= total {
                return Err(PROTOCOL);
            }
            payload.advance(1);
            payload.truncate(total - 1 - pad);
        }
        self.recv_window -= total as i64;
        if self.recv_window < 0 {
            return Err(FLOW_CONTROL);
        }
        let padding = (total - payload.len()) as u32;
        let end = head.flags & frame::END_STREAM != 0;
        let Some(idx) = self.streams.find(head.stream) else {
            self.release_conn(total as u32);
            return self.data_for_unknown(head.stream);
        };
        let s = self.streams.slot(idx);
        if s.reset.is_some() {
            self.release_conn(total as u32);
            return Ok(());
        }
        if s.recv == RecvState::Ended {
            self.release_conn(total as u32);
            return Err(Fail::Stream(head.stream, Reason::STREAM_CLOSED));
        }
        s.recv_window -= total as i32;
        if s.recv_window < 0 {
            self.release_conn(total as u32);
            return Err(Fail::Stream(head.stream, Reason::FLOW_CONTROL_ERROR));
        }
        let len = payload.len() as u64;
        if len > s.content_remaining && s.content_remaining != NO_CONTENT_LENGTH {
            self.release_conn(total as u32);
            return Err(Fail::Stream(head.stream, Reason::PROTOCOL_ERROR));
        }
        if s.content_remaining != NO_CONTENT_LENGTH {
            s.content_remaining -= len;
        }
        if end && s.content_remaining != NO_CONTENT_LENGTH && s.content_remaining != 0 {
            self.release_conn(total as u32);
            return Err(Fail::Stream(head.stream, Reason::PROTOCOL_ERROR));
        }
        // Padding is never seen by the application, so its credit goes back at once.
        if padding > 0 {
            self.release_conn(padding);
            self.release_stream(idx, padding);
        }
        if !end {
            self.account_small_frame(payload.len())?;
        }
        let s = self.streams.slot(idx);
        let nonempty = !payload.is_empty();
        if nonempty {
            s.body().chunks.push_back(Chunk {
                data: payload.freeze(),
                budgeted: !end,
            });
        }
        if end {
            s.recv = RecvState::Ended;
        }
        if nonempty || end {
            if let Some(w) = s.wakers.recv.take() {
                w.wake();
            }
        }
        self.update_open(idx);
        Ok(())
    }

    /// DATA for a stream that is not in the table: ignored after a final GOAWAY or a recent local reset,
    /// otherwise `STREAM_CLOSED` for closed streams and `PROTOCOL_ERROR` for ids never opened.
    fn data_for_unknown(&mut self, id: u32) -> Res {
        if self.goaway_last.is_some_and(|last| id > last) || self.recent_resets.contains(&id) {
            Ok(())
        } else if id > self.highest_id {
            Err(PROTOCOL)
        } else {
            Err(Fail::Stream(id, Reason::STREAM_CLOSED))
        }
    }

    /// Empty frames and frames much smaller than the bookkeeping they cost are metered (`h2`'s data frame budget).
    fn account_small_frame(&mut self, len: usize) -> Res {
        let calm = Fail::Conn(Reason::ENHANCE_YOUR_CALM);
        if len == 0 {
            self.empty_data += 1;
            return if self.empty_data > self.cfg.max_empty_data_frames {
                Err(calm)
            } else {
                Ok(())
            };
        }
        if len < OVERHEAD_THRESHOLD {
            self.data_budget = self
                .data_budget
                .checked_sub(OVERHEAD_THRESHOLD - len)
                .ok_or(calm)?;
        } else {
            self.data_budget =
                (self.data_budget + len - OVERHEAD_THRESHOLD).min(self.cfg.data_frame_budget);
        }
        Ok(())
    }

    // ---- HEADERS / CONTINUATION ----

    fn on_headers(&mut self, head: Head, payload: &[u8]) -> Res {
        if head.stream == 0 {
            return Err(PROTOCOL);
        }
        let mut block = payload;
        if head.flags & frame::PADDED != 0 {
            block = frame::unpad(block).map_err(|()| PROTOCOL)?;
        }
        let self_dependency = if head.flags & frame::PRIORITY_FLAG != 0 {
            let dep = block.get(..5).ok_or(PROTOCOL)?;
            block = &block[5..];
            u32::from_be_bytes([dep[0] & 0x7f, dep[1], dep[2], dep[3]]) == head.stream
        } else {
            false
        };
        let end_stream = head.flags & frame::END_STREAM != 0;
        let mut target = self.classify(head.stream, end_stream)?;
        if self_dependency && matches!(target, Target::New | Target::Trailers(_)) {
            target = Target::Reject(Reason::PROTOCOL_ERROR);
        }
        if head.flags & frame::END_HEADERS != 0 {
            return self.finish_block(head.stream, end_stream, target, block);
        }
        if block.len() > self.cfg.header_block_limit() {
            return Err(Fail::Conn(Reason::ENHANCE_YOUR_CALM));
        }
        self.cont = Some(Cont {
            id: head.stream,
            end_stream,
            target,
            block: block.to_vec(),
            frames: 0,
        });
        Ok(())
    }

    fn on_continuation(&mut self, head: Head, payload: &[u8]) -> Res {
        let Some(c) = self.cont.as_mut() else {
            return Err(PROTOCOL);
        };
        let end_headers = head.flags & frame::END_HEADERS != 0;
        if !end_headers {
            c.frames += 1;
        }
        if c.frames > self.cfg.continuation_frame_limit()
            || c.block.len() + payload.len() > self.cfg.header_block_limit()
        {
            return Err(Fail::Conn(Reason::ENHANCE_YOUR_CALM));
        }
        c.block.extend_from_slice(payload);
        if !end_headers {
            return Ok(());
        }
        let c = self.cont.take().expect("checked above");
        self.finish_block(c.id, c.end_stream, c.target, &c.block)
    }

    fn classify(&mut self, id: u32, end_stream: bool) -> Result<Target, Fail> {
        if self.goaway_last.is_some_and(|last| id > last) {
            return Ok(Target::Ignore);
        }
        if let Some(idx) = self.streams.find(id) {
            let s = self.streams.slot(idx);
            return Ok(if s.reset.is_some() {
                Target::Ignore
            } else if s.recv == RecvState::Ended {
                Target::Reject(Reason::STREAM_CLOSED)
            } else if end_stream {
                Target::Trailers(idx)
            } else {
                Target::Reject(Reason::PROTOCOL_ERROR)
            });
        }
        if id % 2 == 0 {
            return Err(PROTOCOL);
        }
        if id <= self.highest_id {
            return if self.recent_resets.contains(&id) {
                Ok(Target::Ignore)
            } else {
                Err(PROTOCOL)
            };
        }
        self.highest_id = id;
        Ok(
            if self.open_streams >= self.cfg.max_concurrent_streams as usize {
                Target::Refuse
            } else {
                Target::New
            },
        )
    }

    fn finish_block(&mut self, id: u32, end_stream: bool, target: Target, block: &[u8]) -> Res {
        let compression = |_| Fail::Conn(Reason::COMPRESSION_ERROR);
        let max_list = self.cfg.max_header_list_size as usize;
        match target {
            Target::Ignore => request::skip(&mut self.dec, block).map_err(compression),
            Target::Reject(reason) => {
                request::skip(&mut self.dec, block).map_err(compression)?;
                Err(Fail::Stream(id, reason))
            }
            Target::Refuse => {
                request::skip(&mut self.dec, block).map_err(compression)?;
                Err(Fail::Stream(id, Reason::REFUSED_STREAM))
            }
            Target::New => {
                match request::decode_request(&mut self.dec, block, max_list, end_stream)
                    .map_err(compression)?
                {
                    Block::Request {
                        parts,
                        content_length,
                    } => {
                        self.open_stream(id, parts, content_length, end_stream);
                        Ok(())
                    }
                    Block::TooLarge => {
                        self.respond_too_large(id, end_stream);
                        Ok(())
                    }
                    _ => Err(Fail::Stream(id, Reason::PROTOCOL_ERROR)),
                }
            }
            Target::Trailers(idx) => {
                match request::decode_trailers(&mut self.dec, block, max_list)
                    .map_err(compression)?
                {
                    Block::Trailers(map) => self.on_trailers(idx, map),
                    _ => Err(Fail::Stream(id, Reason::PROTOCOL_ERROR)),
                }
            }
        }
    }

    fn open_stream(
        &mut self,
        id: u32,
        parts: Box<http::request::Parts>,
        content_length: u64,
        end_stream: bool,
    ) {
        let (send, recv) = (
            self.peer_initial_window as i32,
            self.cfg.initial_stream_window as i32,
        );
        let idx = self.streams.insert(id, send, recv);
        let s = self.streams.slot(idx);
        s.content_remaining = content_length;
        if end_stream {
            s.recv = RecvState::Ended;
        }
        self.open_streams += 1;
        self.live_pending += 1;
        self.accept.push_back(Pending {
            id,
            idx,
            parts: Some(parts),
            counted: false,
        });
    }

    /// A request over `max_header_list_size`: answered with 431, never delivered. Like `h2`, the stream is
    /// reset only when the request is still streaming; a finished request is simply closed by the response.
    fn respond_too_large(&mut self, id: u32, end_stream: bool) {
        self.write_response(
            id,
            http::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            &HeaderMap::new(),
            true,
        );
        if end_stream {
            self.count_local_error();
        } else {
            self.stream_error(id, Reason::PROTOCOL_ERROR);
        }
    }

    fn on_trailers(&mut self, idx: u32, trailers: HeaderMap) -> Res {
        let s = self.streams.slot(idx);
        if s.content_remaining != NO_CONTENT_LENGTH && s.content_remaining != 0 {
            return Err(Fail::Stream(s.id, Reason::PROTOCOL_ERROR));
        }
        s.body().trailers = Some(trailers);
        s.recv = RecvState::Ended;
        if let Some(w) = s.wakers.recv.take() {
            w.wake();
        }
        self.update_open(idx);
        Ok(())
    }

    // ---- stream and connection control frames ----

    fn on_priority(&mut self, head: Head, payload: &[u8]) -> Res {
        if head.stream == 0 {
            return Err(PROTOCOL);
        }
        if payload.len() != 5 {
            return Err(Fail::Stream(head.stream, Reason::FRAME_SIZE_ERROR));
        }
        let dep = u32::from_be_bytes([payload[0] & 0x7f, payload[1], payload[2], payload[3]]);
        if dep == head.stream {
            return Err(Fail::Stream(head.stream, Reason::PROTOCOL_ERROR));
        }
        self.ignore_frame()
    }

    fn on_rst_stream(&mut self, head: Head, payload: &[u8]) -> Res {
        if head.stream == 0 {
            return Err(PROTOCOL);
        }
        let code = u32::from_be_bytes(payload.try_into().map_err(|_| FRAME_SIZE)?);
        let Some(idx) = self.streams.find(head.stream) else {
            return if head.stream > self.highest_id
                && self.goaway_last.is_none_or(|l| head.stream <= l)
            {
                Err(PROTOCOL)
            } else {
                Ok(())
            };
        };
        let s = self.streams.slot(idx);
        if s.reset.is_some() {
            return Ok(());
        }
        if !s.accepted {
            if self.pending_resets >= self.cfg.max_pending_accept_reset_streams {
                return Err(Fail::Conn(Reason::ENHANCE_YOUR_CALM));
            }
            self.pending_resets += 1;
        } else if !s.is_closed() {
            self.peer_resets += 1;
            if self.peer_resets > self.cfg.max_peer_reset_streams {
                return Err(Fail::Conn(Reason::ENHANCE_YOUR_CALM));
            }
        }
        self.reset_stream(idx, Reason::from(code), Initiator::Remote);
        Ok(())
    }

    fn on_settings(&mut self, head: Head, payload: &[u8]) -> Res {
        if head.stream != 0 {
            return Err(PROTOCOL);
        }
        if head.flags & frame::ACK != 0 {
            if !payload.is_empty() {
                return Err(FRAME_SIZE);
            }
            self.acks_pending = self.acks_pending.checked_sub(1).ok_or(PROTOCOL)?;
            return Ok(());
        }
        let settings = Settings::parse(payload).map_err(|e| Fail::Conn(e.0))?;
        self.got_settings = true;
        self.apply_settings(&settings)?;
        frame::settings_ack(&mut self.out.tail);
        self.control_frame();
        Ok(())
    }

    fn apply_settings(&mut self, s: &Settings) -> Res {
        if let Some(size) = s.max_frame_size {
            self.peer_max_frame = size as usize;
        }
        if let Some(size) = s.header_table_size {
            // TODO(hpack): call `Encoder::set_max_table_size(size)` once the encoder has it.
            let _ = size;
        }
        if let Some(initial) = s.initial_window_size {
            let delta = i64::from(initial) - self.peer_initial_window;
            self.peer_initial_window = i64::from(initial);
            for slot in self.streams.iter_mut() {
                let window = i64::from(slot.send_window) + delta;
                if window > i64::from(MAX_WINDOW) {
                    return Err(FLOW_CONTROL);
                }
                slot.send_window = window as i32;
                if delta > 0 {
                    if let Some(w) = slot.wakers.send.take() {
                        w.wake();
                    }
                }
            }
        }
        Ok(())
    }

    fn on_ping(&mut self, head: Head, payload: &[u8]) -> Res {
        if head.stream != 0 {
            return Err(PROTOCOL);
        }
        let payload: [u8; 8] = payload.try_into().map_err(|_| FRAME_SIZE)?;
        if head.flags & frame::ACK == 0 {
            frame::ping(&mut self.out.tail, payload, true);
            self.control_frame();
        } else if payload == SHUTDOWN_PING && self.shutdown == Shutdown::Draining {
            frame::go_away(&mut self.out.tail, self.highest_id, 0);
            self.goaway_last = Some(self.highest_id);
            self.shutdown = Shutdown::GoAwaySent;
        }
        Ok(())
    }

    fn on_goaway(&mut self, head: Head) -> Res {
        if head.stream != 0 {
            return Err(PROTOCOL);
        }
        if head.len < 8 {
            return Err(FRAME_SIZE);
        }
        self.peer_goaway = true;
        Ok(())
    }

    fn on_window_update(&mut self, head: Head, payload: &[u8]) -> Res {
        let inc = u32::from_be_bytes(payload.try_into().map_err(|_| FRAME_SIZE)?) & 0x7fff_ffff;
        if head.stream == 0 {
            if inc == 0 {
                return Err(PROTOCOL);
            }
            self.send_window += i64::from(inc);
            if self.send_window > i64::from(MAX_WINDOW) {
                return Err(FLOW_CONTROL);
            }
            self.wake_cap_waiters();
            return Ok(());
        }
        let Some(idx) = self.streams.find(head.stream) else {
            return if head.stream > self.highest_id
                && self.goaway_last.is_none_or(|l| head.stream <= l)
            {
                Err(PROTOCOL)
            } else {
                Ok(())
            };
        };
        let s = self.streams.slot(idx);
        if s.reset.is_some() {
            return Ok(());
        }
        let window = i64::from(s.send_window) + i64::from(inc);
        if inc == 0 {
            return Err(Fail::Stream(head.stream, Reason::PROTOCOL_ERROR));
        }
        if window > i64::from(MAX_WINDOW) {
            return Err(Fail::Stream(head.stream, Reason::FLOW_CONTROL_ERROR));
        }
        s.send_window = window as i32;
        if let Some(w) = s.wakers.send.take() {
            w.wake();
        }
        Ok(())
    }
}
