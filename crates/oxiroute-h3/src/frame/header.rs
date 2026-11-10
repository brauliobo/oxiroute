//! Reading a frame header that may be split across chunk boundaries.
use bytes::{Buf, Bytes};

use crate::varint;

/// Two variable-length integers take at most 16 bytes.
const MAX_HEADER: usize = 16;

/// Accumulates the bytes of one frame header (type and length); holds nothing between headers.
#[derive(Default)]
pub struct HeaderReader {
    buf: [u8; MAX_HEADER],
    len: usize,
}

impl HeaderReader {
    /// The `(type, length)` of the next frame once `src` supplies all of its header bytes, consuming them. A header
    /// that `src` ends inside is kept here and completed by the next call.
    #[inline]
    pub fn read(&mut self, src: &mut Bytes) -> Option<(u64, u64)> {
        if self.len == 0 {
            let mut view = &src[..];
            if let (Some(kind), Some(len)) = (varint::decode(&mut view), varint::decode(&mut view))
            {
                let consumed = src.len() - view.len();
                src.advance(consumed);
                return Some((kind, len));
            }
        }
        self.read_split(src)
    }

    #[cold]
    fn read_split(&mut self, src: &mut Bytes) -> Option<(u64, u64)> {
        while self.len < MAX_HEADER && src.has_remaining() {
            self.buf[self.len] = src.get_u8();
            self.len += 1;
            let mut view = &self.buf[..self.len];
            if let (Some(kind), Some(len)) = (varint::decode(&mut view), varint::decode(&mut view))
            {
                self.len = 0;
                return Some((kind, len));
            }
        }
        None
    }

    /// Whether some header bytes arrived but not all of them (the stream must not end here).
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.len > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u64, len: u64) -> Vec<u8> {
        let mut out = Vec::new();
        super::super::write_header(&mut out, kind, len);
        out
    }

    #[test]
    fn whole_header_in_one_chunk_leaves_the_payload() {
        let mut bytes = header(1, 5);
        bytes.extend_from_slice(b"hello");
        let mut src = Bytes::from(bytes);
        let mut reader = HeaderReader::default();
        assert_eq!(reader.read(&mut src), Some((1, 5)));
        assert_eq!(&src[..], b"hello");
        assert!(!reader.is_partial());
    }

    #[test]
    fn header_split_at_every_byte_is_reassembled() {
        let bytes = header(0x1f * 7 + 0x21, 70_000);
        assert_eq!(bytes.len(), 2 + 4);
        let mut reader = HeaderReader::default();
        for (i, byte) in bytes.iter().enumerate() {
            let mut src = Bytes::copy_from_slice(&[*byte]);
            let got = reader.read(&mut src);
            assert!(src.is_empty());
            if i + 1 < bytes.len() {
                assert_eq!(got, None);
                assert!(reader.is_partial());
            } else {
                assert_eq!(got, Some((0x1f * 7 + 0x21, 70_000)));
                assert!(!reader.is_partial());
            }
        }
    }

    #[test]
    fn split_header_keeps_the_bytes_that_follow_it() {
        let bytes = header(0, 3);
        let mut reader = HeaderReader::default();
        assert_eq!(reader.read(&mut Bytes::copy_from_slice(&bytes[..1])), None);
        let mut rest = Bytes::from([&bytes[1..], b"abc"].concat());
        assert_eq!(reader.read(&mut rest), Some((0, 3)));
        assert_eq!(&rest[..], b"abc");
    }
}
