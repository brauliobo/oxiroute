//! Safe process launching and authenticated channels for supervised Linux workers.
//!
//! Workers are always started through `oxiroute-worker-launcher`. The launcher receives the sole
//! control endpoint as fd 0 under an empty fixed environment, marks every unrelated fd at or above
//! 3 `CLOEXEC`, verifies that state, and only then decodes bounded, non-secret worker arguments and
//! environment from argv and spawns the worker. Dynamic-loader variables therefore never affect
//! the launcher itself.
//!
//! On Linux, a writable delegated cgroup-v2 root gives each worker a bounded, insertion-identified
//! cgroup. The launcher attaches itself before creating the worker, so all descendants inherit the
//! cgroup even if they later escape the process group. Forced cleanup uses `cgroup.kill`; pinned
//! process-group signaling remains the fallback when delegation is unavailable or a cgroup write
//! fails. Non-Linux behavior is unchanged.
//!
//! A shared reaper thread is started before any launcher is spawned. `Drop` sends the pinned group
//! `SIGKILL`, invalidates its PGID, and transfers the launcher [`Child`] to that thread without
//! waiting. The receiver loop has no intentional exit path; if it nevertheless terminates, the
//! child handle is deliberately leaked rather than making `Drop` unbounded, leaving zombie cleanup
//! to process exit.

use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read},
    os::{
        fd::{BorrowedFd, OwnedFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::PermissionsExt,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use oxiroute_supervision::{GenerationId, Sequence};
use oxiroute_supervision_unix::{
    Frame, FrameFlags, FrameHeader, InstanceToken, MessageType, PeerIdentity, SeqpacketEndpoint,
    SpawnHandshakeNonce, TransportError,
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
    process::{Pid, Signal, getpgid, kill_process_group},
};
use thiserror::Error;
use zeroize::Zeroizing;

mod reaper;

pub mod cgroup;

pub use cgroup::{
    CgroupV2DelegationStatus, CgroupV2Probe, CgroupV2ProbeStatus, DEFAULT_CGROUP_V2_ROOT,
    probe_cgroup_v2, probe_cgroup_v2_at, probe_cgroup_v2_at_with_controllers,
};

use cgroup::WorkerCgroupLease;
use reaper::{ensure_reaper, submit_cgroup_to_reaper, submit_to_reaper};

#[cfg(target_os = "linux")]
pub mod launcher;

const CHALLENGE: MessageType = MessageType(0xff00);
const READY: MessageType = MessageType(0xff01);
const AUTH_PREFIX_SIZE: usize = 34;
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(5);
const CHILD_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CHANNEL_CLOSE_EXIT_GRACE: Duration = Duration::from_millis(100);
const WORKER_METADATA_VERSION: &str = "v2";

/// Maximum worker argument count encoded for the launcher.
pub const MAX_WORKER_ARGUMENTS: usize = 128;
/// Maximum configured worker environment entry count.
pub const MAX_WORKER_ENVIRONMENT: usize = 128;
/// Maximum bytes in one argument, environment key, or environment value.
pub const MAX_WORKER_METADATA_ITEM_BYTES: usize = 4 * 1024;
/// Maximum aggregate worker argument and environment bytes.
pub const MAX_WORKER_METADATA_BYTES: usize = 64 * 1024;

static STDIN_ADOPTED: AtomicBool = AtomicBool::new(false);

type WorkerEnvironment = Vec<(OsString, OsString)>;

mod command;
mod error;
mod handshake;
mod process;
mod spawn;

pub use command::*;
pub use error::*;
use handshake::{
    authenticate_frame, authenticated_payload, child_pid, encode_hex, generate_nonce,
    parse_authenticated_payload, receive_startup_frame, signal_group, verify_startup_ready,
    wait_for_early_exit,
};
pub use process::*;
pub use spawn::*;

#[cfg(test)]
mod tests;
