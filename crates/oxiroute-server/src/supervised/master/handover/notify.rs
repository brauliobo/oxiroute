use std::{
    io,
    mem::MaybeUninit,
    os::{
        fd::AsFd as _,
        linux::net::SocketAddrExt as _,
        unix::{
            ffi::OsStrExt as _,
            net::{SocketAddr, UnixDatagram},
        },
    },
    time::Duration,
};

use rustix::{
    cmsg_space,
    event::{PollFd, PollFlags, Timespec, poll},
    io::IoSlice,
    net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg},
    pipe::{PipeFlags, pipe_with},
};

const INVOCATION_ID: &str = "INVOCATION_ID";
const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";
const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether the master runs as a systemd service that cannot be told about a new main process.
///
/// Without `NotifyAccess=` the unit tracks the master it started, and ends the service when that
/// master exits after the hand-over.
pub(super) fn unit_ignores_main_pid_changes() -> bool {
    std::env::var_os(INVOCATION_ID).is_some_and(|id| !id.is_empty())
        && std::env::var_os(NOTIFY_SOCKET).is_none()
}

/// Makes the service manager track `pid` as the main process, and returns once it has taken
/// the change into account. A master outside a service manager has nobody to tell.
pub(super) fn announce_main_pid(pid: u32) -> io::Result<()> {
    let Some(path) = std::env::var_os(NOTIFY_SOCKET) else {
        return Ok(());
    };
    let socket = UnixDatagram::unbound()?;
    socket.connect_addr(&notify_address(path.as_bytes())?)?;
    socket.send(format!("MAINPID={pid}").as_bytes())?;
    wait_for_barrier(&socket)
}

fn notify_address(path: &[u8]) -> io::Result<SocketAddr> {
    match path.strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name),
        None => SocketAddr::from_pathname(std::ffi::OsStr::from_bytes(path)),
    }
}

/// The service manager closes the descriptor of a `BARRIER=1` message once it has read every
/// message sent before it.
fn wait_for_barrier(socket: &UnixDatagram) -> io::Result<()> {
    let (acknowledged, held) = pipe_with(PipeFlags::CLOEXEC)?;
    let mut space = [MaybeUninit::uninit(); cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    let descriptors = [held.as_fd()];
    ancillary.push(SendAncillaryMessage::ScmRights(&descriptors));
    sendmsg(
        socket,
        &[IoSlice::new(b"BARRIER=1")],
        &mut ancillary,
        SendFlags::empty(),
    )?;
    drop(held);
    let timeout = Timespec::try_from(BARRIER_TIMEOUT).map_err(io::Error::other)?;
    let mut ready = [PollFd::new(&acknowledged, PollFlags::IN)];
    match poll(&mut ready, Some(&timeout))? {
        0 => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the service manager did not answer",
        )),
        _ => Ok(()),
    }
}
