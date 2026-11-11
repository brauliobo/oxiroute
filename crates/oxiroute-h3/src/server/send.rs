//! Frames of a response: `HEADERS`, `DATA` and trailers laid out in one buffer.
use bytes::Bytes;
use http::{HeaderMap, Response};

use crate::frame;
use crate::qpack::encode;
use crate::varint;

/// Room kept in front of a `HEADERS` block for its frame header (the type and a length of up to eight bytes).
const HEAD_ROOM: usize = 9;

/// Bodies up to this size are copied next to their headers and written in one go; larger ones are passed on as they
/// are.
pub(super) const COPY_LIMIT: usize = 16 * 1024;

/// A response head encoded once: for responses that repeat (fixed routes), sent with
/// [`super::RequestStream::send_prepared`].
#[derive(Clone, Debug)]
pub struct PreparedHead {
    frame: Bytes,
}

impl PreparedHead {
    /// Encodes the head of `response` (status and fields; the body of a `Response<()>` is empty).
    #[must_use]
    pub fn new(response: &Response<()>) -> Self {
        Self {
            frame: Frames::response(Vec::new(), response).to_bytes(),
        }
    }
}

/// A buffer being filled with the frames of a response.
pub(super) struct Frames {
    buffer: Vec<u8>,
    start: usize,
}

impl Frames {
    /// Starts a `HEADERS` frame with the response head.
    pub fn response(mut buffer: Vec<u8>, response: &Response<()>) -> Self {
        buffer.resize(HEAD_ROOM, 0);
        encode::response(&mut buffer, response.status(), response.headers());
        Self::seal_headers(buffer)
    }

    /// Starts with an already encoded `HEADERS` frame.
    pub fn prepared(mut buffer: Vec<u8>, head: &PreparedHead) -> Self {
        buffer.clear();
        buffer.extend_from_slice(&head.frame);
        Self { buffer, start: 0 }
    }

    pub fn trailers(mut buffer: Vec<u8>, trailers: &HeaderMap) -> Self {
        buffer.resize(HEAD_ROOM, 0);
        encode::trailers(&mut buffer, trailers);
        Self::seal_headers(buffer)
    }

    /// An empty set of frames, for a `DATA` frame alone.
    pub fn empty(mut buffer: Vec<u8>) -> Self {
        buffer.clear();
        Self { buffer, start: 0 }
    }

    /// Writes the frame header into the room in front of the block.
    fn seal_headers(mut buffer: Vec<u8>) -> Self {
        let (length, n) = varint::encode_array((buffer.len() - HEAD_ROOM) as u64);
        let start = HEAD_ROOM - 1 - n;
        buffer[start] = frame::HEADERS as u8;
        buffer[start + 1..HEAD_ROOM].copy_from_slice(&length[..n]);
        Self { buffer, start }
    }

    /// Appends the header of a `DATA` frame of `len` bytes.
    pub fn data_header(&mut self, len: usize) {
        frame::write_header(&mut self.buffer, frame::DATA, len as u64);
    }

    /// Appends a whole `DATA` frame.
    pub fn data(&mut self, body: &[u8]) {
        self.data_header(body.len());
        self.buffer.extend_from_slice(body);
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buffer[self.start..]
    }

    pub fn to_bytes(&self) -> Bytes {
        Bytes::copy_from_slice(self.bytes())
    }

    pub fn into_buffer(self) -> Vec<u8> {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::HeaderReader;
    use http::StatusCode;

    fn response(status: StatusCode) -> Response<()> {
        Response::builder().status(status).body(()).unwrap()
    }

    #[test]
    fn headers_frame_header_has_the_shortest_length_encoding() {
        let frames = Frames::response(Vec::new(), &response(StatusCode::OK));
        assert_eq!(frames.bytes(), [0x01, 0x03, 0x00, 0x00, 0xd9]);
        let mut big = Response::new(());
        big.headers_mut()
            .insert("x-big", "v".repeat(100).parse().unwrap());
        let frames = Frames::response(Vec::new(), &big);
        let mut src = Bytes::copy_from_slice(frames.bytes());
        let (kind, len) = HeaderReader::default().read(&mut src).unwrap();
        assert_eq!((kind, len as usize), (frame::HEADERS, src.len()));
        assert_eq!(frames.bytes()[1] & 0xc0, 0x40, "two-byte length");
    }

    #[test]
    fn data_frames_follow_the_headers() {
        let mut frames = Frames::response(Vec::new(), &response(StatusCode::OK));
        let head_len = frames.bytes().len();
        frames.data(b"hello");
        assert_eq!(
            &frames.bytes()[head_len..],
            [0x00, 0x05, b'h', b'e', b'l', b'l', b'o']
        );
    }
}
