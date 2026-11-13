// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use bytes::{Bytes, BytesMut};

/// Chunks of at least this many bytes are kept as the (reference counted) buffer they were read into; shorter ones are
/// copied into a joined tail. A chunk handed over from a socket read keeps its whole read buffer alive, so keeping a
/// short one would pin many times its size.
const SHARE_MIN_BYTES: usize = 64 * 1024;

/// A buffer with size limit. When the total amount of data written to the buffer is below the limit
/// all the data will be held in the buffer. Otherwise, the buffer will report to be truncated and let go of its data.
///
/// Large chunks are held as written, without a copy; reading back more than one piece joins them.
pub struct FixedBuffer {
    shared: Vec<Bytes>,
    tail: BytesMut,
    len: usize,
    capacity: usize,
    truncated: bool,
}

impl FixedBuffer {
    pub fn new(capacity: usize) -> Self {
        FixedBuffer {
            shared: Vec::new(),
            tail: BytesMut::new(),
            len: 0,
            capacity,
            truncated: false,
        }
    }

    /// A buffer that has already let go of its data and keeps none.
    pub fn truncated() -> Self {
        FixedBuffer {
            truncated: true,
            ..Self::new(0)
        }
    }

    pub fn write_to_buffer(&mut self, data: &Bytes) {
        if self.truncated || self.len + data.len() > self.capacity {
            self.truncated = true;
            self.clear_data();
            return;
        }
        self.len += data.len();
        if data.len() >= SHARE_MIN_BYTES {
            if !self.tail.is_empty() {
                self.shared.push(self.tail.split().freeze());
            }
            self.shared.push(data.clone());
        } else {
            self.tail.extend_from_slice(data);
        }
    }

    fn clear_data(&mut self) {
        self.len = 0;
        self.shared = Vec::new();
        self.tail = BytesMut::new();
    }

    pub fn clear(&mut self) {
        self.truncated = false;
        self.len = 0;
        self.shared.clear();
        self.tail.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
    pub fn get_buffer(&self) -> Option<Bytes> {
        match (self.shared.as_slice(), self.tail.is_empty()) {
            ([], true) => None,
            ([], false) => Some(self.tail.clone().freeze()),
            ([chunk], true) => Some(chunk.clone()),
            (shared, _) => {
                let mut joined = BytesMut::with_capacity(self.len);
                shared
                    .iter()
                    .for_each(|chunk| joined.extend_from_slice(chunk));
                joined.extend_from_slice(&self.tail);
                Some(joined.freeze())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_chunks_are_joined_in_order_and_a_large_chunk_is_shared() {
        let mut buffer = FixedBuffer::new(4 * SHARE_MIN_BYTES);
        assert!(buffer.get_buffer().is_none());
        buffer.write_to_buffer(&Bytes::from_static(b"abc"));
        buffer.write_to_buffer(&Bytes::new());
        buffer.write_to_buffer(&Bytes::from_static(b"def"));
        assert_eq!(&buffer.get_buffer().unwrap()[..], b"abcdef");
        assert!(!buffer.is_truncated() && !buffer.is_empty());

        buffer.clear();
        let large = Bytes::from(vec![b'x'; SHARE_MIN_BYTES]);
        buffer.write_to_buffer(&large);
        assert_eq!(buffer.get_buffer().unwrap().as_ptr(), large.as_ptr());
        buffer.write_to_buffer(&Bytes::from_static(b"yz"));
        let joined = buffer.get_buffer().unwrap();
        assert_eq!(joined.len(), SHARE_MIN_BYTES + 2);
        assert_eq!(&joined[SHARE_MIN_BYTES - 1..], b"xyz");
    }

    #[test]
    fn a_truncated_buffer_keeps_nothing_it_is_given() {
        let mut buffer = FixedBuffer::truncated();
        buffer.write_to_buffer(&Bytes::from_static(b"abc"));
        assert!(buffer.is_truncated() && buffer.is_empty());
        assert!(buffer.get_buffer().is_none());
    }

    #[test]
    fn a_chunk_over_the_capacity_truncates_and_releases_the_data() {
        let mut buffer = FixedBuffer::new(4);
        buffer.write_to_buffer(&Bytes::from_static(b"abc"));
        buffer.write_to_buffer(&Bytes::from_static(b"de"));
        assert!(buffer.is_truncated() && buffer.is_empty());
        assert_eq!(buffer.tail.capacity(), 0);
        buffer.write_to_buffer(&Bytes::from_static(b"a"));
        assert!(buffer.is_truncated());
        buffer.clear();
        buffer.write_to_buffer(&Bytes::from_static(b"abcd"));
        assert!(!buffer.is_truncated());
        assert_eq!(&buffer.get_buffer().unwrap()[..], b"abcd");
    }
}
