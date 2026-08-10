#![no_main]

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use oxiroute_forward_proxy::OverreadIo;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

const MAX_INPUT_BYTES: usize = 16_384;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES || data.is_empty() {
        return;
    }

    let rest = &data[1..];
    let split = usize::from(data[0]) % (rest.len() + 1);
    let prefix = &rest[..split];
    let socket_bytes = &rest[split..];
    let mut expected = Vec::with_capacity(rest.len());
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(socket_bytes);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    runtime.block_on(async {
        let mut reader = OverreadIo::new(
            MemoryReader {
                bytes: socket_bytes,
                offset: 0,
            },
            Bytes::copy_from_slice(prefix),
        );
        let mut actual = Vec::new();
        reader
            .read_to_end(&mut actual)
            .await
            .expect("memory reader cannot fail");
        assert_eq!(actual, expected);
    });
});

struct MemoryReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl AsyncRead for MemoryReader<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let remaining = &self.bytes[self.offset..];
        let length = remaining.len().min(buffer.remaining());
        buffer.put_slice(&remaining[..length]);
        self.offset += length;
        Poll::Ready(Ok(()))
    }
}
