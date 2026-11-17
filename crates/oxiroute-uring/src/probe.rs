use std::{fmt, fs, io, time::Duration};

use io_uring::opcode;
use rustix::{
    io::Errno,
    net::{AddressFamily, SocketFlags, SocketType, socketpair},
};

use crate::{
    config::{Config, Taskrun},
    event::{Event, Read},
    ring::Ring,
};

const SYSCTL: &str = "/proc/sys/kernel/io_uring_disabled";
const SELF_TEST: Duration = Duration::from_secs(2);

/// A capability the data plane needs that this kernel or sandbox does not offer.
#[derive(Debug)]
pub enum Missing {
    /// `kernel.io_uring_disabled` is 1 (restricted to a group) or 2 (disabled).
    Disabled { sysctl: u8 },
    /// The kernel has no `io_uring` system calls.
    Kernel,
    /// A seccomp filter or container policy refuses the system call.
    Refused,
    /// The ring could not be created with the settings the plane uses.
    Setup {
        what: &'static str,
        error: io::Error,
    },
    /// An opcode the plane submits is unknown to the kernel.
    Opcode(&'static str),
    /// The registered buffer ring or a receive that selects from it does not work.
    ProvidedBuffers(io::Error),
    /// The self test ran and failed.
    SelfTest(&'static str),
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled { sysctl: 2 } => {
                write!(
                    f,
                    "the kernel disables io_uring (kernel.io_uring_disabled = 2)"
                )
            }
            Self::Disabled { sysctl } => write!(
                f,
                "the kernel restricts io_uring to a group this process is not in (kernel.io_uring_disabled = {sysctl})"
            ),
            Self::Kernel => write!(f, "the kernel has no io_uring support"),
            Self::Refused => write!(
                f,
                "a seccomp filter or container policy refuses the io_uring system calls"
            ),
            Self::Setup { what, error } => write!(f, "the kernel cannot set up {what}: {error}"),
            Self::Opcode(name) => write!(
                f,
                "the kernel does not support the io_uring {name} operation"
            ),
            Self::ProvidedBuffers(error) => {
                write!(f, "provided receive buffers do not work: {error}")
            }
            Self::SelfTest(step) => write!(f, "the io_uring self test failed at {step}"),
        }
    }
}

impl std::error::Error for Missing {}

fn sysctl() -> Option<u8> {
    fs::read_to_string(SYSCTL).ok()?.trim().parse().ok()
}

fn setup_failure(error: io::Error, taskrun: Taskrun) -> Missing {
    match error.raw_os_error().map(Errno::from_raw_os_error) {
        Some(Errno::NOSYS) => Missing::Kernel,
        Some(Errno::PERM | Errno::ACCESS) => sysctl()
            .filter(|setting| *setting > 0)
            .map_or(Missing::Refused, |sysctl| Missing::Disabled { sysctl }),
        _ => Missing::Setup {
            what: match taskrun {
                Taskrun::Defer => "a single-issuer ring with deferred task running (kernel 6.1)",
                Taskrun::Cooperative => {
                    "a single-issuer ring with cooperative task running (kernel 5.19)"
                }
            },
            error,
        },
    }
}

fn required(ring: &Ring) -> Result<(), Missing> {
    let probe = ring.opcodes().map_err(|error| Missing::Setup {
        what: "the opcode probe (kernel 5.6)",
        error,
    })?;
    let needs = [
        (opcode::Accept::CODE, "accept"),
        (opcode::Connect::CODE, "connect"),
        (opcode::Recv::CODE, "recv"),
        (opcode::Send::CODE, "send"),
        (opcode::PollAdd::CODE, "poll"),
        (opcode::AsyncCancel::CODE, "cancel"),
    ];
    match needs.iter().find(|(code, _)| !probe.is_supported(*code)) {
        Some((_, name)) => Err(Missing::Opcode(name)),
        None => Ok(()),
    }
}

fn echo(ring: &mut Ring) -> Result<(), Missing> {
    let (left, right) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|error| Missing::Setup {
        what: "the self test sockets",
        error: error.into(),
    })?;
    let (receiver, sender) = (ring.adopt(left, 1), ring.adopt(right, 2));
    ring.start(receiver);
    let queued = ring.send(sender, b"pro", b"be");
    let result = awaited(ring, receiver, queued);
    ring.close(receiver);
    ring.close(sender);
    result
}

fn awaited(ring: &mut Ring, receiver: crate::Handle, queued: crate::Sent) -> Result<(), Missing> {
    if queued != crate::Sent::Queued(5) {
        return Err(Missing::SelfTest("send"));
    }
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, Event::Readable { tag: 1 }))
    {
        events.clear();
        ring.wait(Some(SELF_TEST), &mut events)
            .map_err(Missing::ProvidedBuffers)?;
        if events.is_empty() {
            return Err(Missing::SelfTest("receive with a provided buffer"));
        }
    }
    let mut bytes = [0_u8; 8];
    match ring.read(receiver, &mut bytes) {
        Read::Data(5) if &bytes[..5] == b"probe" => Ok(()),
        _ => Err(Missing::SelfTest("reading the provided buffer")),
    }
}

/// Checks that a ring configured like `config` works here: the sysctl, the system calls, every opcode the plane
/// submits, and a round trip through the provided-buffer ring. Run it at plan time and refuse the configuration
/// when it fails.
pub fn probe(config: &Config) -> Result<(), Missing> {
    if let Some(setting @ 2) = sysctl() {
        return Err(Missing::Disabled { sysctl: setting });
    }
    let test = Config {
        entries: 8,
        buffers: 2,
        buffer_size: 4096,
        large_buffers: 2,
        large_buffer_size: 4096,
        accepts: 1,
        ..config.clone()
    };
    let mut ring = Ring::new(&test).map_err(|error| setup_failure(error, config.taskrun))?;
    required(&ring)?;
    echo(&mut ring)
}
