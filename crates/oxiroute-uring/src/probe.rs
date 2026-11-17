use std::{fmt, fs, io, time::Duration};

use io_uring::opcode;
use rustix::{
    io::Errno,
    net::{AddressFamily, SocketFlags, SocketType, socketpair},
    process::{Resource, getrlimit},
};

use crate::{
    config::{Config, Taskrun},
    event::{Event, Read},
    ring::Ring,
    stage::{SETTING_UP_THE_RING, Stage},
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
    /// The process cannot get the memory of a ring with the settings the plane uses: an address space or memory limit
    /// (`RLIMIT_AS`, a cgroup `memory.max`) or the kernel's own allocations.
    Memory(io::Error),
    /// The kernel refused the memory of the ring itself while `RLIMIT_MEMLOCK` is finite: kernels that account ring
    /// memory to the user count `ring` bytes of every ring against that limit, which all processes of the user share.
    Locked {
        limit: u64,
        ring: u64,
        error: io::Error,
    },
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
            Self::Memory(error) => write!(
                f,
                "the process cannot get the memory of an io_uring ring and its receive buffers: {error}"
            ),
            Self::Locked { limit, ring, error } => write!(
                f,
                "the kernel refused the memory of an io_uring ring ({} KiB; a kernel that accounts ring memory to the user \
                 counts it against RLIMIT_MEMLOCK of {} KiB, shared by every process of the user, one ring per engine \
                 thread): {error}",
                ring.div_ceil(1024),
                limit / 1024
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

fn ring_memory(error: io::Error, config: &Config, memlock: Option<u64>) -> Missing {
    match memlock {
        Some(limit) => Missing::Locked {
            limit,
            ring: config.ring_bytes(),
            error,
        },
        None => Missing::Memory(error),
    }
}

fn setup_failure(error: io::Error, config: &Config, memlock: Option<u64>) -> Missing {
    let stage = Stage::of(&error);
    let errno = stage
        .map_or_else(|| error.raw_os_error(), Stage::errno)
        .map(Errno::from_raw_os_error);
    match errno {
        Some(Errno::NOSYS) => Missing::Kernel,
        Some(Errno::PERM | Errno::ACCESS) => sysctl()
            .filter(|setting| *setting > 0)
            .map_or(Missing::Refused, |sysctl| Missing::Disabled { sysctl }),
        Some(Errno::NOMEM) if stage.is_some_and(|stage| stage.is(SETTING_UP_THE_RING)) => {
            ring_memory(error, config, memlock)
        }
        _ if error.kind() == io::ErrorKind::OutOfMemory => Missing::Memory(error),
        _ => Missing::Setup {
            what: match config.taskrun {
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

/// Checks that a ring configured exactly like `config` works here: the sysctl, the system calls, the memory of the ring
/// and of its provided buffers, every opcode the plane submits, and a round trip through the provided-buffer ring. Run
/// it at plan time and refuse the configuration when it fails.
pub fn probe(config: &Config) -> Result<(), Missing> {
    if let Some(setting @ 2) = sysctl() {
        return Err(Missing::Disabled { sysctl: setting });
    }
    let memlock = getrlimit(Resource::Memlock).current;
    let mut ring = Ring::new(config).map_err(|error| setup_failure(error, config, memlock))?;
    required(&ring)?;
    echo(&mut ring)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::during;

    fn failed(step: &'static str, errno: Errno) -> io::Error {
        during(step)(io::Error::from_raw_os_error(errno.raw_os_error()))
    }

    #[test]
    fn a_refused_ring_under_a_finite_memlock_names_the_limit_and_the_ring_size() {
        let error = failed(SETTING_UP_THE_RING, Errno::NOMEM);
        let refusal = setup_failure(error, &Config::default(), Some(8 << 20));
        assert!(
            matches!(
                refusal,
                Missing::Locked {
                    limit: 8_388_608,
                    ring: 217_088,
                    ..
                }
            ),
            "{refusal}"
        );
        let text = refusal.to_string();
        assert!(
            text.contains("RLIMIT_MEMLOCK of 8192 KiB") && text.contains("212 KiB"),
            "{text}"
        );
    }

    #[test]
    fn a_refused_ring_without_a_memlock_limit_is_a_memory_refusal() {
        let error = failed(SETTING_UP_THE_RING, Errno::NOMEM);
        assert!(matches!(
            setup_failure(error, &Config::default(), None),
            Missing::Memory(_)
        ));
    }

    #[test]
    fn buffers_the_process_cannot_map_are_a_memory_refusal_whatever_memlock_is() {
        let error =
            during("allocating the small receive buffers")(io::ErrorKind::OutOfMemory.into());
        assert!(matches!(
            setup_failure(error, &Config::default(), Some(8 << 20)),
            Missing::Memory(_)
        ));
    }

    #[test]
    fn the_errno_of_a_failed_step_still_selects_its_refusal() {
        let refusal =
            |errno| setup_failure(failed(SETTING_UP_THE_RING, errno), &Config::default(), None);
        assert!(matches!(refusal(Errno::NOSYS), Missing::Kernel));
        assert!(matches!(refusal(Errno::INVAL), Missing::Setup { .. }));
    }
}
