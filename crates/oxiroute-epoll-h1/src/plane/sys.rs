use std::{io, net::SocketAddr, os::fd::OwnedFd};

use rustix::{
    event::epoll,
    fs::{self, Mode, OFlags},
    io::{Errno, IoSlice},
    net::{
        self, AddressFamily, RecvFlags, SendAncillaryBuffer, SendFlags, Shutdown, SocketFlags,
        SocketType, sockopt,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Read {
    Data(usize),
    Eof,
    Block,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Wrote {
    Bytes(usize),
    Block,
    Failed,
}

pub(crate) fn read(fd: &OwnedFd, buf: &mut [u8]) -> Read {
    loop {
        return match net::recv(fd, &mut *buf, RecvFlags::empty()) {
            Ok((_, 0)) => Read::Eof,
            Ok((_, count)) => Read::Data(count),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => Read::Block,
            Err(_) => Read::Failed,
        };
    }
}

pub(crate) fn peek(fd: &OwnedFd) -> Read {
    let mut byte = [0_u8; 1];
    loop {
        return match net::recv(fd, &mut byte[..], RecvFlags::PEEK | RecvFlags::DONTWAIT) {
            Ok((_, 0)) => Read::Eof,
            Ok(_) => Read::Data(1),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => Read::Block,
            Err(_) => Read::Failed,
        };
    }
}

pub(crate) fn write(fd: &OwnedFd, head: &[u8], body: &[u8]) -> Wrote {
    if !head.is_empty() {
        return write_vectored(fd, &[IoSlice::new(head), IoSlice::new(body)]);
    }
    loop {
        return match net::send(fd, body, SendFlags::NOSIGNAL) {
            Ok(count) => Wrote::Bytes(count),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => Wrote::Block,
            Err(_) => Wrote::Failed,
        };
    }
}

pub(crate) fn write_vectored(fd: &OwnedFd, slices: &[IoSlice<'_>]) -> Wrote {
    loop {
        return match net::sendmsg(
            fd,
            slices,
            &mut SendAncillaryBuffer::default(),
            SendFlags::NOSIGNAL,
        ) {
            Ok(count) => Wrote::Bytes(count),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => Wrote::Block,
            Err(_) => Wrote::Failed,
        };
    }
}

pub(crate) fn shutdown_write(fd: &OwnedFd) {
    let _ = net::shutdown(fd, Shutdown::Write);
}

pub(crate) enum Accepted {
    Connection(OwnedFd, Option<SocketAddr>),
    Empty,
    Exhausted,
    Failed,
}

pub(crate) fn accept(listener: &OwnedFd) -> Accepted {
    match net::acceptfrom_with(listener, SocketFlags::NONBLOCK | SocketFlags::CLOEXEC) {
        Ok((fd, peer)) => {
            Accepted::Connection(fd, peer.and_then(|peer| SocketAddr::try_from(peer).ok()))
        }
        Err(Errno::AGAIN) => Accepted::Empty,
        Err(Errno::MFILE | Errno::NFILE) => Accepted::Exhausted,
        Err(_) => Accepted::Failed,
    }
}

pub(crate) fn spare_descriptor() -> io::Result<OwnedFd> {
    Ok(fs::open(
        "/dev/null",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

pub(crate) struct Connecting {
    pub(crate) fd: OwnedFd,
    pub(crate) established: bool,
}

fn family(address: &SocketAddr) -> AddressFamily {
    if address.is_ipv4() {
        AddressFamily::INET
    } else {
        AddressFamily::INET6
    }
}

pub(crate) fn unconnected(address: &SocketAddr) -> io::Result<OwnedFd> {
    let fd = net::socket_with(
        family(address),
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    sockopt::set_tcp_nodelay(&fd, true)?;
    Ok(fd)
}

pub(crate) fn connect(address: &SocketAddr) -> io::Result<Connecting> {
    let fd = unconnected(address)?;
    match net::connect(&fd, address) {
        Ok(()) => Ok(Connecting {
            fd,
            established: true,
        }),
        Err(Errno::INPROGRESS) => Ok(Connecting {
            fd,
            established: false,
        }),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn connect_result(fd: &OwnedFd) -> bool {
    matches!(sockopt::socket_error(fd), Ok(Ok(())))
}

pub(crate) fn bind_group(address: SocketAddr, count: usize) -> io::Result<Vec<OwnedFd>> {
    (0..count)
        .map(|_| {
            let fd = net::socket_with(
                family(&address),
                SocketType::STREAM,
                SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
                None,
            )?;
            sockopt::set_socket_reuseaddr(&fd, true)?;
            sockopt::set_socket_reuseport(&fd, true)?;
            net::bind(&fd, &address)?;
            net::listen(&fd, 4096)?;
            Ok(fd)
        })
        .collect()
}

pub(crate) fn prepare_listener(fd: &OwnedFd) -> io::Result<()> {
    sockopt::set_tcp_nodelay(fd, true)?;
    Ok(())
}

pub(crate) fn interest_flags(
    trigger: super::config::Trigger,
    read: bool,
    write: bool,
) -> epoll::EventFlags {
    match trigger {
        super::config::Trigger::Edge => {
            epoll::EventFlags::IN
                | epoll::EventFlags::OUT
                | epoll::EventFlags::RDHUP
                | epoll::EventFlags::ET
        }
        super::config::Trigger::Level => {
            let reading = if read {
                epoll::EventFlags::IN | epoll::EventFlags::RDHUP
            } else {
                epoll::EventFlags::empty()
            };
            let writing = if write {
                epoll::EventFlags::OUT
            } else {
                epoll::EventFlags::empty()
            };
            reading | writing
        }
    }
}

pub(crate) fn rewatch(
    epoll_fd: &OwnedFd,
    fd: &OwnedFd,
    token: u64,
    flags: epoll::EventFlags,
) -> io::Result<()> {
    Ok(epoll::modify(
        epoll_fd,
        fd,
        epoll::EventData::new_u64(token),
        flags,
    )?)
}

pub(crate) fn watch(
    epoll_fd: &OwnedFd,
    fd: &OwnedFd,
    token: u64,
    flags: epoll::EventFlags,
) -> io::Result<()> {
    Ok(epoll::add(
        epoll_fd,
        fd,
        epoll::EventData::new_u64(token),
        flags,
    )?)
}

pub(crate) fn unwatch(epoll_fd: &OwnedFd, fd: &OwnedFd) {
    let _ = epoll::delete(epoll_fd, fd);
}
