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

use super::{Encode, COMPRESSION_ERROR};

use bytes::Bytes;
use flate2::write::GzDecoder;
use flate2::{Compress, Crc, FlushCompress, Status};
use pingora_error::{OrErr, Result};
use std::cell::RefCell;
use std::io::Write;
use std::time::{Duration, Instant};

pub struct Decompressor {
    decompress: GzDecoder<Vec<u8>>,
    total_in: usize,
    total_out: usize,
    duration: Duration,
}

impl Decompressor {
    pub fn new() -> Self {
        Decompressor {
            decompress: GzDecoder::new(vec![]),
            total_in: 0,
            total_out: 0,
            duration: Duration::new(0, 0),
        }
    }
}

impl Encode for Decompressor {
    fn encode(&mut self, input: &[u8], end: bool) -> Result<Bytes> {
        const MAX_INIT_COMPRESSED_SIZE_CAP: usize = 4 * 1024;
        const ESTIMATED_COMPRESSION_RATIO: usize = 3; // estimated 2.5-3x compression
        let start = Instant::now();
        self.total_in += input.len();
        // cap the buf size amplification, there is a DoS risk of always allocate
        // 3x the memory of the input buffer
        let reserve_size = if input.len() < MAX_INIT_COMPRESSED_SIZE_CAP {
            input.len() * ESTIMATED_COMPRESSION_RATIO
        } else {
            input.len()
        };
        self.decompress.get_mut().reserve(reserve_size);
        self.decompress
            .write_all(input)
            .or_err(COMPRESSION_ERROR, "while decompress Gzip")?;
        // write to vec will never fail, only possible error is that the input data
        // was not actually gzip compressed
        if end {
            self.decompress
                .try_finish()
                .or_err(COMPRESSION_ERROR, "while decompress Gzip")?;
        }
        self.total_out += self.decompress.get_ref().len();
        self.duration += start.elapsed();
        Ok(std::mem::take(self.decompress.get_mut()).into()) // into() Bytes will drop excess capacity
    }

    fn stat(&self) -> (&'static str, usize, usize, Duration) {
        ("de-gzip", self.total_in, self.total_out, self.duration)
    }
}

/// Deflate states kept per thread. Creating one allocates and clears about 380 KiB; a worker thread compresses
/// one response at a time per connection, so a few idle states cover the steady state.
const POOLED_STATES: usize = 4;

thread_local! {
    static IDLE_STATES: RefCell<Vec<(u32, Compress)>> = const { RefCell::new(Vec::new()) };
}

/// Gzip framing around a raw deflate state that returns to a per-thread pool when the response ends.
pub struct Compressor {
    compress: Option<Compress>,
    crc: Crc,
    level: u32,
    header_pending: bool,
    total_in: usize,
    total_out: usize,
    duration: Duration,
}

impl Compressor {
    pub fn new(level: u32) -> Compressor {
        let idle = IDLE_STATES.with(|idle| {
            let mut idle = idle.borrow_mut();
            let position = idle
                .iter()
                .position(|(idle_level, _)| *idle_level == level)?;
            Some(idle.swap_remove(position).1)
        });
        Compressor {
            compress: Some(
                idle.unwrap_or_else(|| Compress::new(flate2::Compression::new(level), false)),
            ),
            crc: Crc::new(),
            level,
            header_pending: true,
            total_in: 0,
            total_out: 0,
            duration: Duration::new(0, 0),
        }
    }

    fn header(&self) -> [u8; 10] {
        // The level byte and the unknown operating system match the stream of `flate2::write::GzEncoder`.
        let level = flate2::Compression::new(self.level);
        let extra_flags = if level.level() >= flate2::Compression::best().level() {
            2
        } else if level.level() <= flate2::Compression::fast().level() {
            4
        } else {
            0
        };
        [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, extra_flags, 255]
    }

    /// Deflates `input` into `out`, growing it as needed, until everything is consumed (and, when finishing,
    /// the stream has ended).
    fn deflate(compress: &mut Compress, mut input: &[u8], out: &mut Vec<u8>, flush: FlushCompress) {
        // reserve at most 16k at a time
        const GROW: usize = 16 * 1024;
        loop {
            out.reserve(GROW.min(input.len().max(512)));
            let before = compress.total_in();
            // compressing into a vec cannot fail for valid input
            let status = compress.compress_vec(input, out, flush).unwrap();
            input = &input[(compress.total_in() - before) as usize..];
            let full = out.len() == out.capacity();
            match status {
                Status::StreamEnd => return,
                _ if input.is_empty() && !full && flush != FlushCompress::Finish => return,
                _ => {}
            }
        }
    }
}

impl Drop for Compressor {
    fn drop(&mut self) {
        let Some(mut compress) = self.compress.take() else {
            return;
        };
        compress.reset();
        let level = self.level;
        let _ = IDLE_STATES.try_with(|idle| {
            let mut idle = idle.borrow_mut();
            if idle.len() < POOLED_STATES {
                idle.push((level, compress));
            }
        });
    }
}

impl Encode for Compressor {
    // infallible because compression can take any data
    fn encode(&mut self, input: &[u8], end: bool) -> Result<Bytes> {
        const HEADER_AND_TRAILER: usize = 18;
        let start = Instant::now();
        self.total_in += input.len();
        let mut out = Vec::with_capacity(HEADER_AND_TRAILER);
        if self.header_pending {
            self.header_pending = false;
            out.extend_from_slice(&self.header());
        }
        self.crc.update(input);
        let compress = self
            .compress
            .as_mut()
            .expect("deflate state is present until drop");
        Self::deflate(compress, input, &mut out, FlushCompress::None);
        if end {
            Self::deflate(compress, &[], &mut out, FlushCompress::Finish);
            out.extend_from_slice(&self.crc.sum().to_le_bytes());
            out.extend_from_slice(&self.crc.amount().to_le_bytes());
        }
        self.total_out += out.len();
        self.duration += start.elapsed();
        Ok(out.into()) // into() Bytes will drop excess capacity
    }

    fn stat(&self) -> (&'static str, usize, usize, Duration) {
        ("gzip", self.total_in, self.total_out, self.duration)
    }
}

use std::ops::{Deref, DerefMut};
impl Deref for Decompressor {
    type Target = GzDecoder<Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.decompress
    }
}

impl DerefMut for Decompressor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.decompress
    }
}

#[cfg(test)]
mod tests_stream {
    use super::*;

    #[test]
    fn gzip_data() {
        let mut compressor = Compressor::new(6);
        let compressed = compressor.encode(b"abcdefg", true).unwrap();
        // gzip magic headers
        assert_eq!(&compressed[..3], &[0x1f, 0x8b, 0x08]);
        // check the crc32 footer
        assert_eq!(
            &compressed[compressed.len() - 9..],
            &[0, 166, 106, 42, 49, 7, 0, 0, 0]
        );
        assert_eq!(compressor.total_in, 7);
        assert_eq!(compressor.total_out, compressed.len());
    }

    #[test]
    fn the_framing_matches_the_flate2_gzip_encoder() {
        use std::io::Read;
        let input: Vec<u8> = (0..100_000_u32).map(|value| (value % 251) as u8).collect();
        for level in [1, 6, 9] {
            let mut expected =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
            expected.write_all(&input).unwrap();
            let expected = expected.finish().unwrap();
            let mut compressor = Compressor::new(level);
            let mut actual = compressor.encode(&input[..40_000], false).unwrap().to_vec();
            actual.extend_from_slice(&compressor.encode(&input[40_000..], true).unwrap());
            assert_eq!(actual[..10], expected[..10], "header at level {level}");
            assert_eq!(
                actual[actual.len() - 8..],
                expected[expected.len() - 8..],
                "trailer at level {level}"
            );
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(&actual[..])
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded, input, "round trip at level {level}");
        }
    }

    #[test]
    fn pooled_states_start_every_stream_afresh() {
        let input: Vec<u8> = (0..70_000_u32).map(|value| (value % 97) as u8).collect();
        let compress = |level| {
            let mut compressor = Compressor::new(level);
            let mut out = compressor.encode(&input[..30_000], false).unwrap().to_vec();
            out.extend_from_slice(&compressor.encode(&input[30_000..], true).unwrap());
            out
        };
        let first = compress(1);
        assert_eq!(
            compress(1),
            first,
            "a reused state repeats the stream of a new one"
        );
        assert_ne!(compress(9), first);
        assert_eq!(compress(1), first);
        let mut decoder = Decompressor::new();
        assert_eq!(&decoder.encode(&first, true).unwrap()[..], &input[..]);
    }

    #[test]
    fn gunzip_data() {
        let mut decompressor = Decompressor::new();

        let compressed_bytes = &[
            0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 255, 75, 76, 74, 78, 73, 77, 75, 7, 0, 166, 106,
            42, 49, 7, 0, 0, 0,
        ];
        let decompressed = decompressor.encode(compressed_bytes, true).unwrap();

        assert_eq!(&decompressed[..], b"abcdefg");
        assert_eq!(decompressor.total_in, compressed_bytes.len());
        assert_eq!(decompressor.total_out, decompressed.len());

        assert!(decompressor.get_ref().is_empty());
    }
}
