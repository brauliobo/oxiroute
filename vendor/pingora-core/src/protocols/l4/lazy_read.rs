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

//! A buffered stream whose read buffer costs no memory while the connection is idle.
//!
//! `tokio::io::BufStream` keeps one read buffer per connection for its whole life, which makes every
//! idle keep-alive connection hold 64 KiB. Here the socket is read into one scratch buffer shared by
//! all the connections of a thread, and only the bytes the caller did not ask for yet are kept per
//! connection, in a buffer that is released as soon as the socket has nothing more to read.

use std::cell::Cell;
use std::io::IoSlice;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{self, AsyncRead, AsyncWrite, BufWriter, ReadBuf};

thread_local! {
    /// Empty until the first buffered read of the thread. Taken for the length of one `poll_read`.
    static SCRATCH: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

#[derive(Debug)]
pub(crate) struct LazyReadStream<RW> {
    inner: BufWriter<RW>,
    /// Bytes read from the socket and not consumed yet, valid from `pos`.
    pending: Vec<u8>,
    pos: usize,
    /// Size of the shared scratch buffer, and the smallest read that bypasses it.
    read_size: usize,
}

impl<RW: AsyncWrite> LazyReadStream<RW> {
    pub(crate) fn new(read_size: usize, write_capacity: usize, inner: RW) -> Self {
        Self {
            inner: BufWriter::with_capacity(write_capacity, inner),
            pending: Vec::new(),
            pos: 0,
            read_size,
        }
    }

    pub(crate) fn get_ref(&self) -> &RW {
        self.inner.get_ref()
    }

    pub(crate) fn get_mut(&mut self) -> &mut RW {
        self.inner.get_mut()
    }

    pub(crate) fn into_inner(self) -> RW {
        self.inner.into_inner()
    }
}

impl<RW: AsyncRead + AsyncWrite + Unpin> LazyReadStream<RW> {
    fn read_pending(&mut self, buf: &mut ReadBuf<'_>) {
        let available = &self.pending[self.pos..];
        let count = available.len().min(buf.remaining());
        buf.put_slice(&available[..count]);
        self.pos += count;
        if self.pos == self.pending.len() {
            self.pos = 0;
            self.pending.clear();
        }
    }

    fn read_socket(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut scratch = SCRATCH.take();
        scratch.resize(self.read_size, 0);
        let mut read = ReadBuf::new(&mut scratch);
        let result = Pin::new(self.inner.get_mut()).poll_read(cx, &mut read);
        match &result {
            Poll::Ready(Ok(())) => {
                let received = read.filled();
                let count = received.len().min(buf.remaining());
                buf.put_slice(&received[..count]);
                self.pending.extend_from_slice(&received[count..]);
            }
            Poll::Ready(Err(_)) => {}
            // Nothing to read: the connection is idle, so it keeps no buffer.
            Poll::Pending => self.pending = Vec::new(),
        }
        SCRATCH.set(scratch);
        result
    }
}

impl<RW: AsyncRead + AsyncWrite + Unpin> AsyncRead for LazyReadStream<RW> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pos < self.pending.len() {
            self.read_pending(buf);
            return Poll::Ready(Ok(()));
        }
        if buf.remaining() >= self.read_size {
            return Pin::new(self.inner.get_mut()).poll_read(cx, buf);
        }
        self.read_socket(cx, buf)
    }
}

impl<RW: AsyncWrite + Unpin> AsyncWrite for LazyReadStream<RW> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    const READ_SIZE: usize = 64;

    fn stream_over_duplex() -> (
        LazyReadStream<tokio::io::DuplexStream>,
        tokio::io::DuplexStream,
    ) {
        let (near, far) = duplex(1024);
        (LazyReadStream::new(READ_SIZE, 16, near), far)
    }

    #[tokio::test]
    async fn bytes_beyond_the_requested_length_stay_for_the_next_read() {
        let (mut stream, mut far) = stream_over_duplex();
        far.write_all(b"hello world").await.unwrap();
        let mut first = [0; 5];
        stream.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"hello");
        assert_eq!(stream.pending.len() - stream.pos, 6);
        let mut rest = [0; 6];
        stream.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b" world");
        assert!(stream.pending.is_empty());
    }

    #[tokio::test]
    async fn reads_at_least_the_buffer_size_bypass_the_scratch() {
        let (mut stream, mut far) = stream_over_duplex();
        far.write_all(&[7; 200]).await.unwrap();
        let mut large = [0; READ_SIZE];
        stream.read_exact(&mut large).await.unwrap();
        assert!(large.iter().all(|byte| *byte == 7));
        assert!(stream.pending.is_empty());
    }

    #[tokio::test]
    async fn an_idle_connection_releases_its_pending_buffer() {
        let (mut stream, mut far) = stream_over_duplex();
        far.write_all(b"abcdef").await.unwrap();
        let mut head = [0; 2];
        stream.read_exact(&mut head).await.unwrap();
        let mut tail = [0; 4];
        stream.read_exact(&mut tail).await.unwrap();
        assert!(stream.pending.capacity() > 0);
        let idle = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            stream.read_exact(&mut head),
        )
        .await;
        assert!(idle.is_err());
        assert_eq!(stream.pending.capacity(), 0);
    }

    #[tokio::test]
    async fn end_of_stream_reads_zero_bytes() {
        let (mut stream, far) = stream_over_duplex();
        drop(far);
        let mut buf = [0; 8];
        assert_eq!(stream.read(&mut buf).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn writes_stay_buffered_until_flushed() {
        let (mut stream, mut far) = stream_over_duplex();
        stream.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            far.read_exact(&mut buf)
        )
        .await
        .is_err());
        stream.flush().await.unwrap();
        far.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
