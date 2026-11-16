use std::{
    io::{self, IoSlice, IoSliceMut},
    mem::size_of,
    os::fd::AsRawFd,
    pin::Pin,
    task::{Context, Poll, ready},
};

use nix::{
    cmsg_space,
    sys::socket::{ControlMessageOwned, MsgFlags, TlsGetRecordType, recvmsg},
};
use pingora::protocols::{Peek, l4::stream::Stream as L4Stream};
use socket2::{MsgHdr, SockRef};
use tokio::{
    io::{AsyncWrite, Interest, ReadBuf},
    net::TcpStream,
};

/// The largest plaintext of one TLS record.
const MAX_PLAINTEXT: usize = 16 * 1024;
const ALERT_CONTENT_TYPE: u8 = 21;
const ALERT_LEVEL_WARNING: u8 = 1;
const ALERT_CLOSE_NOTIFY: u8 = 0;
const CLOSE_NOTIFY: [u8; 2] = [ALERT_LEVEL_WARNING, ALERT_CLOSE_NOTIFY];

/// `struct cmsghdr` (length, level, type) and one byte of data, padded to the alignment of the next header.
const RECORD_TYPE_CONTROL_LEN: usize = size_of::<libc::cmsghdr>() + 1;
const RECORD_TYPE_CONTROL_SPACE: usize =
    RECORD_TYPE_CONTROL_LEN.next_multiple_of(size_of::<usize>());
const _: () = assert!(size_of::<libc::cmsghdr>() == 16 && size_of::<usize>() == 8);

/// A connection the kernel could not take over after rustls gave its secrets up: nothing can serve it any more.
#[derive(Debug)]
pub(crate) struct Failure {
    kind: io::ErrorKind,
    message: String,
}

impl Failure {
    pub(crate) fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

impl Default for Failure {
    fn default() -> Self {
        Self {
            kind: io::ErrorKind::Interrupted,
            message: "the conversion to kernel TLS did not finish".into(),
        }
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: format!("kernel TLS could not take the connection over: {error}"),
        }
    }
}

/// What one `recvmsg` on a socket whose kernel decrypts the records returned.
enum Record {
    Plaintext(usize),
    Alert(u8),
    Closed,
}

/// A connection whose records the kernel encrypts and decrypts: bytes written to the socket leave as application data
/// records and reads return the plaintext of the ones that arrived. The record types the kernel hands to user space
/// (alerts, handshake messages) are answered here.
#[derive(Debug)]
pub(crate) struct KernelIo {
    control: Vec<u8>,
    close_notify_received: bool,
    close_notify_sent: bool,
}

impl Default for KernelIo {
    fn default() -> Self {
        Self {
            control: cmsg_space!(u8),
            close_notify_received: false,
            close_notify_sent: false,
        }
    }
}

fn socket(io: &L4Stream) -> io::Result<&TcpStream> {
    io.tcp_socket().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "kernel TLS lost its TCP socket",
        )
    })
}

impl KernelIo {
    pub(crate) fn poll_read(
        &mut self,
        io: &L4Stream,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 || self.close_notify_received {
            return Poll::Ready(Ok(()));
        }
        let socket = socket(io)?;
        loop {
            ready!(socket.poll_read_ready(context))?;
            let room = buffer.remaining().min(MAX_PLAINTEXT);
            let plaintext = buffer.initialize_unfilled_to(room);
            let received = socket.try_io(Interest::READABLE, || {
                receive(socket, plaintext, &mut self.control)
            });
            match received {
                Ok(Record::Plaintext(length)) => {
                    buffer.advance(length);
                    return Poll::Ready(Ok(()));
                }
                Ok(Record::Closed) => return Poll::Ready(Ok(())),
                Ok(Record::Alert(ALERT_CLOSE_NOTIFY)) => {
                    self.close_notify_received = true;
                    return Poll::Ready(Ok(()));
                }
                Ok(Record::Alert(description)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        format!("the peer sent TLS alert {description}"),
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
    }

    pub(crate) fn poll_write(
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(io).poll_write(context, buffer)
    }

    pub(crate) fn poll_write_vectored(
        io: &mut L4Stream,
        context: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(io).poll_write_vectored(context, buffers)
    }

    pub(crate) fn poll_flush(io: &mut L4Stream, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(io).poll_flush(context)
    }

    /// Flushes what the transport holds, sends `close_notify` as the one alert record the kernel cannot send by
    /// itself, and closes the write side.
    pub(crate) fn poll_shutdown(
        &mut self,
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        ready!(Pin::new(&mut *io).poll_flush(context))?;
        if !self.close_notify_sent {
            ready!(send_close_notify(socket(io)?, context))?;
            self.close_notify_sent = true;
        }
        Pin::new(io).poll_shutdown(context)
    }

    pub(crate) fn poll_read_ready(
        io: &mut L4Stream,
        context: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        io.poll_read_ready(context)
    }
}

fn receive(socket: &TcpStream, plaintext: &mut [u8], control: &mut [u8]) -> io::Result<Record> {
    let (length, record_type) = {
        let mut buffers = [IoSliceMut::new(&mut *plaintext)];
        let message = recvmsg::<()>(
            socket.as_raw_fd(),
            &mut buffers,
            Some(control),
            MsgFlags::empty(),
        )?;
        let record_type = message.cmsgs()?.find_map(|message| match message {
            ControlMessageOwned::TlsGetRecordType(record_type) => Some(record_type),
            _ => None,
        });
        (message.bytes, record_type)
    };
    match (record_type, length) {
        (_, 0) => Ok(Record::Closed),
        (Some(TlsGetRecordType::ApplicationData), length) => Ok(Record::Plaintext(length)),
        (Some(TlsGetRecordType::Alert), 2) => Ok(Record::Alert(plaintext[1])),
        (Some(TlsGetRecordType::Handshake), _) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the peer sent a TLS handshake message (key update or renegotiation) on a connection the kernel decrypts",
        )),
        (record_type, _) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected TLS record from the kernel: {record_type:?}"),
        )),
    }
}

fn send_close_notify(socket: &TcpStream, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    let control = record_type_control(ALERT_CONTENT_TYPE);
    let alert = [IoSlice::new(&CLOSE_NOTIFY)];
    loop {
        ready!(socket.poll_write_ready(context))?;
        let sent = socket.try_io(Interest::WRITABLE, || {
            let message = MsgHdr::new().with_buffers(&alert).with_control(&control);
            SockRef::from(socket).sendmsg(&message, 0)
        });
        match sent {
            Ok(_) => return Poll::Ready(Ok(())),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Poll::Ready(Err(error)),
        }
    }
}

/// The `TLS_SET_RECORD_TYPE` control message that makes the kernel send the data of a `sendmsg` as a record of
/// another type than application data.
fn record_type_control(content_type: u8) -> [u8; RECORD_TYPE_CONTROL_SPACE] {
    let mut control = [0; RECORD_TYPE_CONTROL_SPACE];
    let header = size_of::<libc::cmsghdr>();
    control[..8].copy_from_slice(&RECORD_TYPE_CONTROL_LEN.to_ne_bytes());
    control[8..12].copy_from_slice(&libc::SOL_TLS.to_ne_bytes());
    control[12..16].copy_from_slice(&libc::TLS_SET_RECORD_TYPE.to_ne_bytes());
    control[header] = content_type;
    control
}
