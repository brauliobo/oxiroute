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

/// A buffer with size limit. When the total amount of data written to the buffer is below the limit
/// all the data will be held in the buffer. Otherwise, the buffer will report to be truncated and let go of its data.
///
/// The data is held as the chunks it was written as (reference counted, not copied); only reading back more than one
/// chunk joins them.
pub struct FixedBuffer {
    chunks: Vec<Bytes>,
    len: usize,
    capacity: usize,
    truncated: bool,
}

impl FixedBuffer {
    pub fn new(capacity: usize) -> Self {
        FixedBuffer {
            chunks: Vec::new(),
            len: 0,
            capacity,
            truncated: false,
        }
    }

    pub fn write_to_buffer(&mut self, data: &Bytes) {
        if !self.truncated && (self.len + data.len() <= self.capacity) {
            if !data.is_empty() {
                self.len += data.len();
                self.chunks.push(data.clone());
            }
        } else {
            self.truncated = true;
            self.len = 0;
            self.chunks = Vec::new();
        }
    }
    pub fn clear(&mut self) {
        self.truncated = false;
        self.len = 0;
        self.chunks.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
    pub fn get_buffer(&self) -> Option<Bytes> {
        match self.chunks.as_slice() {
            [] => None,
            [chunk] => Some(chunk.clone()),
            chunks => {
                let mut joined = BytesMut::with_capacity(self.len);
                chunks
                    .iter()
                    .for_each(|chunk| joined.extend_from_slice(chunk));
                Some(joined.freeze())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_joined_in_order_and_one_chunk_is_shared() {
        let mut buffer = FixedBuffer::new(16);
        assert!(buffer.get_buffer().is_none());
        let first = Bytes::from_static(b"abc");
        buffer.write_to_buffer(&first);
        assert_eq!(buffer.get_buffer().unwrap().as_ptr(), first.as_ptr());
        buffer.write_to_buffer(&Bytes::new());
        buffer.write_to_buffer(&Bytes::from_static(b"def"));
        assert_eq!(&buffer.get_buffer().unwrap()[..], b"abcdef");
        assert!(!buffer.is_truncated() && !buffer.is_empty());
    }

    #[test]
    fn a_chunk_over_the_capacity_truncates_and_releases_the_data() {
        let mut buffer = FixedBuffer::new(4);
        buffer.write_to_buffer(&Bytes::from_static(b"abc"));
        buffer.write_to_buffer(&Bytes::from_static(b"de"));
        assert!(buffer.is_truncated() && buffer.is_empty());
        buffer.write_to_buffer(&Bytes::from_static(b"a"));
        assert!(buffer.is_truncated());
        buffer.clear();
        buffer.write_to_buffer(&Bytes::from_static(b"abcd"));
        assert!(!buffer.is_truncated());
        assert_eq!(&buffer.get_buffer().unwrap()[..], b"abcd");
    }
}
