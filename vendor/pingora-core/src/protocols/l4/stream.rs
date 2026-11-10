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

//! Transport layer connection

use async_trait::async_trait;
use futures::FutureExt;
use log::debug;

use pingora_error::{ErrorType::*, OrErr, Result};
#[cfg(target_os = "linux")]
use std::any::Any;
use std::io::IoSliceMut;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};
#[cfg(target_os = "linux")]
use tokio::io::Interest;
use tokio::io::{self, AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::protocols::l4::ext::{set_tcp_keepalive, TcpKeepalive};
use crate::protocols::l4::lazy_read::LazyReadStream;
use crate::protocols::l4::virt;
use crate::protocols::raw_connect::ProxyDigest;
use crate::protocols::{
    GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek, Shutdown, SocketDigest, Ssl,
    TimingDigest, UniqueID, UniqueIDType,
};
use crate::upstreams::peer::Tracer;

#[derive(Debug)]
enum RawStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    Virtual(virt::VirtualSocketStream),
}

impl AsyncRead for RawStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_read(cx, buf),
            }
        }
    }
}

impl AsyncWrite for RawStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write(cx, buf),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_flush(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_flush(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_flush(cx),
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_shutdown(cx),
            }
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            RawStream::Tcp(s) => s.is_write_vectored(),
            #[cfg(unix)]
            RawStream::Unix(s) => s.is_write_vectored(),
            RawStream::Virtual(s) => s.is_write_vectored(),
        }
    }
}

#[cfg(unix)]
impl AsRawFd for RawStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        match self {
            RawStream::Tcp(s) => s.as_raw_fd(),
            RawStream::Unix(s) => s.as_raw_fd(),
            RawStream::Virtual(_) => -1, // Virtual stream does not have a real fd
        }
    }
}

#[cfg(windows)]
impl AsRawSocket for RawStream {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        match self {
            RawStream::Tcp(s) => s.as_raw_socket(),
            // Virtual stream does not have a real socket, return INVALID_SOCKET (!0)
            RawStream::Virtual(_) => !0,
        }
    }
}

#[derive(Debug)]
struct RawStreamWrapper {
    pub(crate) stream: RawStream,
    /// store the last rx timestamp of the stream.
    pub(crate) rx_ts: Option<SystemTime>,
    /// enable reading rx timestamp
    #[cfg(target_os = "linux")]
    pub(crate) enable_rx_ts: bool,
    #[cfg(target_os = "linux")]
    /// This can be reused across multiple recvmsg calls. The cmsg buffer may
    /// come from old sockets created by older version of pingora and so,
    /// this vector can only grow.
    reusable_cmsg_space: Vec<u8>,
}

impl RawStream {
    /// Registers the socket with the I/O driver of the runtime this is called on, instead of the one that accepted it.
    fn into_current_runtime(self) -> io::Result<Self> {
        Ok(match self {
            Self::Tcp(s) => Self::Tcp(TcpStream::from_std(s.into_std()?)?),
            #[cfg(unix)]
            Self::Unix(s) => Self::Unix(UnixStream::from_std(s.into_std()?)?),
            Self::Virtual(s) => Self::Virtual(s),
        })
    }
}

impl RawStreamWrapper {
    fn into_current_runtime(self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.into_current_runtime()?,
            ..self
        })
    }

    pub fn new(stream: RawStream) -> Self {
        RawStreamWrapper {
            stream,
            rx_ts: None,
            #[cfg(target_os = "linux")]
            enable_rx_ts: false,
            #[cfg(target_os = "linux")]
            reusable_cmsg_space: nix::cmsg_space!(nix::sys::time::TimeSpec),
        }
    }

    #[cfg(target_os = "linux")]
    pub fn enable_rx_ts(&mut self, enable_rx_ts: bool) {
        self.enable_rx_ts = enable_rx_ts;
    }
}

impl AsyncRead for RawStreamWrapper {
    #[cfg(not(target_os = "linux"))]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            let rs_wrapper = Pin::get_unchecked_mut(self);
            match &mut rs_wrapper.stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_read(cx, buf),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        use futures::ready;
        use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags, SockaddrStorage};

        // if we do not need rx timestamp, then use the standard path
        if !self.enable_rx_ts {
            // Safety: Basic enum pin projection
            unsafe {
                let rs_wrapper = Pin::get_unchecked_mut(self);
                match &mut rs_wrapper.stream {
                    RawStream::Tcp(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                    RawStream::Unix(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                    RawStream::Virtual(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                }
            }
        }

        // Safety: Basic pin projection to get mutable stream
        let rs_wrapper = unsafe { Pin::get_unchecked_mut(self) };
        match &mut rs_wrapper.stream {
            RawStream::Tcp(s) => {
                loop {
                    ready!(s.poll_read_ready(cx))?;
                    // Safety: maybe uninitialized bytes will only be passed to recvmsg
                    let b = unsafe {
                        &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>]
                            as *mut [u8])
                    };
                    let mut iov = [IoSliceMut::new(b)];
                    rs_wrapper.reusable_cmsg_space.clear();

                    match s.try_io(Interest::READABLE, || {
                        recvmsg::<SockaddrStorage>(
                            s.as_raw_fd(),
                            &mut iov,
                            Some(&mut rs_wrapper.reusable_cmsg_space),
                            MsgFlags::empty(),
                        )
                        .map_err(|errno| errno.into())
                    }) {
                        Ok(r) => {
                            if let Some(ControlMessageOwned::ScmTimestampsns(rtime)) = r
                                .cmsgs()
                                .find(|i| matches!(i, ControlMessageOwned::ScmTimestampsns(_)))
                            {
                                // The returned timestamp is a real (i.e. not monotonic) timestamp
                                // https://docs.kernel.org/networking/timestamping.html
                                rs_wrapper.rx_ts =
                                    SystemTime::UNIX_EPOCH.checked_add(rtime.system.into());
                            }
                            // Safety: We trust `recvmsg` to have filled up `r.bytes` bytes in the buffer.
                            unsafe {
                                buf.assume_init(r.bytes);
                            }
                            buf.advance(r.bytes);
                            return Poll::Ready(Ok(()));
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                }
            }
            // Unix RX timestamp only works with datagram for now, so we do not care about it
            RawStream::Unix(s) => unsafe { Pin::new_unchecked(s).poll_read(cx, buf) },
            RawStream::Virtual(s) => unsafe { Pin::new_unchecked(s).poll_read(cx, buf) },
        }
    }
}

impl AsyncWrite for RawStreamWrapper {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write(cx, buf),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_flush(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_flush(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_flush(cx),
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_shutdown(cx),
            }
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

#[cfg(unix)]
impl AsRawFd for RawStreamWrapper {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.stream.as_raw_fd()
    }
}

#[cfg(windows)]
impl AsRawSocket for RawStreamWrapper {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.stream.as_raw_socket()
    }
}

// Large read buffering helps reducing syscalls with little trade-off
// Ssl layer always does "small" reads in 16k (TLS record size) so L4 read buffer helps a lot.
// The buffer is a thread-shared scratch (see `lazy_read`), so an idle connection holds none of it.
const BUF_READ_SIZE: usize = 64 * 1024;
// Small write buf to match MSS. Too large write buf delays real time communication.
// This buffering effectively implements something similar to Nagle's algorithm.
// The benefit is that user space can control when to flush, where Nagle's can't be controlled.
// And userspace buffering reduce both syscalls and small packets.
const BUF_WRITE_SIZE: usize = 1460;
// tokio rejects zero-capacity buffers. One byte keeps every read and write bypassing the buffer
// until `set_buffer` installs the real sizes.
const UNBUFFERED_CAPACITY: usize = 1;

// NOTE: with writer buffering, users need to call flush() to make sure the data is actually
// sent. Otherwise data could be stuck in the buffer forever or get lost when stream is closed.

/// A concrete type for transport layer connection + extra fields for logging
#[derive(Debug)]
pub struct Stream {
    // Declared first so it drops before the socket: a closed connection never still holds its slot.
    admission: Option<Box<dyn Any + Send + Sync>>,
    // Use `Option` to be able to swap to adjust the buffer size. Always safe to unwrap
    stream: Option<LazyReadStream<RawStreamWrapper>>,
    // the data put back at the front of the read buffer, in order to replay the read
    rewind_read_buf: Vec<Vec<u8>>,
    buffer_write: bool,
    proxy_digest: Option<Arc<ProxyDigest>>,
    socket_digest: Option<Arc<SocketDigest>>,
    /// When this connection is established
    pub established_ts: SystemTime,
    /// The distributed tracing object for this stream
    pub tracer: Option<Tracer>,
    /// Last rx timestamp associated with the last recvmsg call.
    pub rx_ts: Option<SystemTime>,
}

impl Stream {
    /// Ties application admission state to this connection: it is released right before the
    /// socket closes, so a client that reconnects on seeing the close is never rejected by it.
    pub fn hold_admission(&mut self, admission: Box<dyn Any + Send + Sync>) {
        self.admission = Some(admission);
    }

    fn stream(&self) -> &LazyReadStream<RawStreamWrapper> {
        self.stream.as_ref().expect("stream should always be set")
    }

    fn stream_mut(&mut self) -> &mut LazyReadStream<RawStreamWrapper> {
        self.stream.as_mut().expect("stream should always be set")
    }

    /// set TCP nodelay for this connection if `self` is TCP
    pub fn set_nodelay(&mut self) -> Result<()> {
        match &self.stream_mut().get_mut().stream {
            RawStream::Tcp(s) => {
                s.set_nodelay(true)
                    .or_err(ConnectError, "failed to set_nodelay")?;
            }
            RawStream::Virtual(s) => {
                s.set_socket_option(virt::VirtualSockOpt::NoDelay)
                    .or_err(ConnectError, "failed to set_nodelay on virtual socket")?;
            }
            _ => (),
        }
        Ok(())
    }

    /// set TCP keepalive settings for this connection if `self` is TCP
    pub fn set_keepalive(&mut self, ka: &TcpKeepalive) -> Result<()> {
        match &self.stream_mut().get_mut().stream {
            RawStream::Tcp(s) => {
                debug!("Setting tcp keepalive");
                set_tcp_keepalive(s, ka)?;
            }
            RawStream::Virtual(s) => {
                s.set_socket_option(virt::VirtualSockOpt::KeepAlive(ka.clone()))
                    .or_err(ConnectError, "failed to set_keepalive on virtual socket")?;
            }
            _ => (),
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn set_rx_timestamp(&mut self) -> Result<()> {
        use nix::sys::socket::{setsockopt, sockopt, TimestampingFlag};

        if let RawStream::Tcp(s) = &self.stream_mut().get_mut().stream {
            let timestamp_options = TimestampingFlag::SOF_TIMESTAMPING_RX_SOFTWARE
                | TimestampingFlag::SOF_TIMESTAMPING_SOFTWARE;
            setsockopt(s.as_raw_fd(), sockopt::Timestamping, &timestamp_options)
                .or_err(InternalError, "failed to set SOF_TIMESTAMPING_RX_SOFTWARE")?;
            self.stream_mut().get_mut().enable_rx_ts(true);
        }

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn set_rx_timestamp(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Put Some data back to the head of the stream to be read again
    pub(crate) fn rewind(&mut self, data: &[u8]) {
        if !data.is_empty() {
            self.rewind_read_buf.push(data.to_vec());
        }
    }

    /// Set the buffer of the stream
    /// It is only set later because of the malloc overhead in critical accept() path
    ///
    /// With `own_driver` the socket also moves to the I/O driver of the calling runtime, for a connection that was
    /// accepted by another runtime: its readiness events then arrive on the thread that serves it.
    pub(crate) fn set_buffer(&mut self, own_driver: bool) -> io::Result<()> {
        // The buffer sizes are fixed at construction, so take the raw stream out of the
        // unbuffered stream and wrap it again with the sizes we want
        let raw = self
            .stream
            .take()
            .expect("stream should always be set")
            .into_inner();
        let raw = if own_driver {
            raw.into_current_runtime()?
        } else {
            raw
        };
        self.stream = Some(LazyReadStream::new(BUF_READ_SIZE, BUF_WRITE_SIZE, raw));
        Ok(())
    }
}

impl From<TcpStream> for Stream {
    fn from(s: TcpStream) -> Self {
        Stream {
            admission: None,
            stream: Some(LazyReadStream::new(
                UNBUFFERED_CAPACITY,
                UNBUFFERED_CAPACITY,
                RawStreamWrapper::new(RawStream::Tcp(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            rx_ts: None,
        }
    }
}

impl From<virt::VirtualSocketStream> for Stream {
    fn from(s: virt::VirtualSocketStream) -> Self {
        Stream {
            admission: None,
            stream: Some(LazyReadStream::new(
                UNBUFFERED_CAPACITY,
                UNBUFFERED_CAPACITY,
                RawStreamWrapper::new(RawStream::Virtual(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            rx_ts: None,
        }
    }
}

#[cfg(unix)]
impl From<UnixStream> for Stream {
    fn from(s: UnixStream) -> Self {
        Stream {
            admission: None,
            stream: Some(LazyReadStream::new(
                UNBUFFERED_CAPACITY,
                UNBUFFERED_CAPACITY,
                RawStreamWrapper::new(RawStream::Unix(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            rx_ts: None,
        }
    }
}

#[cfg(unix)]
impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.stream().get_ref().as_raw_fd()
    }
}

#[cfg(windows)]
impl AsRawSocket for Stream {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.stream().get_ref().as_raw_socket()
    }
}

#[cfg(unix)]
impl UniqueID for Stream {
    fn id(&self) -> UniqueIDType {
        self.as_raw_fd()
    }
}

#[cfg(windows)]
impl UniqueID for Stream {
    fn id(&self) -> usize {
        self.as_raw_socket() as usize
    }
}

impl Ssl for Stream {}

#[async_trait]
impl Peek for Stream {
    async fn try_peek(&mut self, buf: &mut [u8]) -> std::io::Result<bool> {
        use tokio::io::AsyncReadExt;
        self.read_exact(buf).await?;
        // rewind regardless of what is read
        self.rewind(buf);
        Ok(true)
    }

    fn poll_read_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.rewind_read_buf.is_empty() || self.stream().has_buffered() {
            return Poll::Ready(Ok(()));
        }
        match &self.stream().get_ref().stream {
            RawStream::Tcp(s) => s.poll_read_ready(cx),
            #[cfg(unix)]
            RawStream::Unix(s) => s.poll_read_ready(cx),
            RawStream::Virtual(_) => Poll::Ready(Ok(())),
        }
    }
}

#[async_trait]
impl Shutdown for Stream {
    async fn shutdown(&mut self) {
        AsyncWriteExt::shutdown(self).await.unwrap_or_else(|e| {
            debug!("Failed to shutdown connection: {:?}", e);
        });
    }
}

impl GetTimingDigest for Stream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        let mut digest = Vec::with_capacity(2); // expect to have both L4 stream and TLS layer
        digest.push(Some(TimingDigest {
            established_ts: self.established_ts,
        }));
        digest
    }

    fn get_read_pending_time(&self) -> Duration {
        Duration::ZERO
    }

    fn get_write_pending_time(&self) -> Duration {
        Duration::ZERO
    }
}

impl GetProxyDigest for Stream {
    fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
        self.proxy_digest.clone()
    }

    fn set_proxy_digest(&mut self, digest: ProxyDigest) {
        self.proxy_digest = Some(Arc::new(digest));
    }
}

impl GetSocketDigest for Stream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        self.socket_digest.clone()
    }

    fn set_socket_digest(&mut self, socket_digest: SocketDigest) {
        self.socket_digest = Some(Arc::new(socket_digest))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(t) = self.tracer.as_ref() {
            t.0.on_disconnected();
        }
        // Try to flush the write buffer. We use now_or_never() because
        // 1. Drop cannot be async
        // 2. write should usually be ready, unless the buf is full.
        // A broken socket fails the flush; probing it first would cost a syscall per connection.
        if let Some(Err(e)) = self.flush().now_or_never() {
            debug!("Socket is already broken {:?}", e);
        }
        debug!("Dropping socket {:?}", self.stream);
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let result = if !self.rewind_read_buf.is_empty() {
            let data_to_read = self.rewind_read_buf.pop().unwrap(); // safe
            let mut data_to_read = data_to_read.as_slice();
            let result = Pin::new(&mut data_to_read).poll_read(cx, buf);
            // return the remaining data back to the head of rewind_read_buf
            if !data_to_read.is_empty() {
                let remaining_buf = Vec::from(data_to_read);
                self.rewind_read_buf.push(remaining_buf);
            }
            result
        } else {
            Pin::new(&mut self.stream_mut()).poll_read(cx, buf)
        };
        self.rx_ts = self.stream().get_ref().rx_ts;
        result
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.buffer_write {
            Pin::new(&mut self.stream_mut()).poll_write(cx, buf)
        } else {
            Pin::new(&mut self.stream_mut().get_mut()).poll_write(cx, buf)
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream_mut()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream_mut()).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.buffer_write {
            Pin::new(&mut self.stream_mut()).poll_write_vectored(cx, bufs)
        } else {
            Pin::new(&mut self.stream_mut().get_mut()).poll_write_vectored(cx, bufs)
        }
    }

    fn is_write_vectored(&self) -> bool {
        if self.buffer_write {
            self.stream().is_write_vectored() // it is true
        } else {
            self.stream().get_ref().is_write_vectored()
        }
    }
}

pub mod async_write_vec;

pub use async_write_vec::AsyncWriteVec;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
