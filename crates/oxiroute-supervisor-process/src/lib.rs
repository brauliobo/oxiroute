//! Safe process launching and authenticated channels for supervised Linux workers.
//!
//! Workers are always started through `oxiroute-worker-launcher`. The launcher receives the sole
//! control endpoint as fd 0 under an empty fixed environment, marks every unrelated fd at or above
//! 3 `CLOEXEC`, verifies that state, and only then decodes bounded, non-secret worker arguments and
//! environment from argv and spawns the worker. Dynamic-loader variables therefore never affect
//! the launcher itself.
//!
//! The launcher remains the dedicated process-group leader and supervises the actual worker. Group
//! signaling is permitted only while the parent still owns the unreaped launcher [`Child`]; reaping
//! atomically invalidates the numeric PGID. This covers ordinary descendants that remain in that
//! group. It cannot contain a descendant that deliberately calls `setsid` or moves to another
//! process group; cgroup-backed containment is intentionally deferred to a later integration slice.
//! If the launcher is killed alone rather than through this API, it cannot perform its normal group
//! cleanup; observing and reaping that launcher deliberately invalidates the PGID instead of risking
//! a signal to a reused group, so surviving processes then require an external cgroup owner.
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
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
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

const CHALLENGE: MessageType = MessageType(0xff00);
const READY: MessageType = MessageType(0xff01);
const AUTH_PREFIX_SIZE: usize = 34;
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(5);
const CHILD_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CHANNEL_CLOSE_EXIT_GRACE: Duration = Duration::from_millis(100);

/// Maximum worker argument count encoded for the launcher.
pub const MAX_WORKER_ARGUMENTS: usize = 128;
/// Maximum configured worker environment entry count.
pub const MAX_WORKER_ENVIRONMENT: usize = 128;
/// Maximum bytes in one argument, environment key, or environment value.
pub const MAX_WORKER_METADATA_ITEM_BYTES: usize = 4 * 1024;
/// Maximum aggregate worker argument and environment bytes.
pub const MAX_WORKER_METADATA_BYTES: usize = 64 * 1024;

static STDIN_ADOPTED: AtomicBool = AtomicBool::new(false);
static REAPER: OnceLock<Reaper> = OnceLock::new();

/// Public, non-secret identity carried in every authenticated frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerIdentity {
    /// Runtime instance token.
    pub instance: InstanceToken,
    /// Service generation.
    pub generation: GenerationId,
    /// Application protocol version.
    pub protocol: u16,
}

/// A resolved worker executable plus explicitly configured non-secret launch metadata.
///
/// Arguments and environment are encoded into the launcher's visible argv. They are bounded and
/// must never contain credentials, nonces, tokens, or other secrets.
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl WorkerCommand {
    /// Resolves an executable before the sanitized launcher environment is created.
    ///
    /// Absolute and path-containing inputs are canonicalized directly. Bare names are resolved
    /// against the caller's current `PATH`, then stored as an absolute canonical path.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable cannot be resolved to an executable regular file.
    pub fn new(program: impl AsRef<OsStr>) -> Result<Self, ExecutableError> {
        Ok(Self {
            program: resolve_executable(program.as_ref())?,
            args: Vec::new(),
            env: Vec::new(),
        })
    }

    /// Appends non-secret worker mode metadata as one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl AsRef<OsStr>) -> Self {
        self.args.push(argument.as_ref().to_owned());
        self
    }

    /// Adds one explicitly configured non-secret environment entry.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Allowlists one variable from the parent environment when it is present.
    #[must_use]
    pub fn inherit_env(mut self, key: impl AsRef<OsStr>) -> Self {
        let key = key.as_ref();
        if let Some(value) = env::var_os(key) {
            self.env.push((key.to_owned(), value));
        }
        self
    }

    fn into_launcher_command(
        self,
        launcher: &Path,
        endpoint: SeqpacketEndpoint,
    ) -> Result<Command, WorkerMetadataError> {
        self.validate_metadata()?;
        let mut command = Command::new(launcher);
        command
            .arg(self.program)
            .arg(self.args.len().to_string())
            .args(self.args.iter().map(|value| encode_hex(value.as_bytes())))
            .arg(self.env.len().to_string());
        for (key, value) in self.env {
            command
                .arg(encode_hex(key.as_bytes()))
                .arg(encode_hex(value.as_bytes()));
        }
        command
            .env_clear()
            .stdin(Stdio::from(endpoint.into_owned_fd()))
            .process_group(0);
        Ok(command)
    }

    fn validate_metadata(&self) -> Result<(), WorkerMetadataError> {
        if self.args.len() > MAX_WORKER_ARGUMENTS {
            return Err(WorkerMetadataError::TooManyArguments {
                actual: self.args.len(),
                maximum: MAX_WORKER_ARGUMENTS,
            });
        }
        if self.env.len() > MAX_WORKER_ENVIRONMENT {
            return Err(WorkerMetadataError::TooManyEnvironmentEntries {
                actual: self.env.len(),
                maximum: MAX_WORKER_ENVIRONMENT,
            });
        }
        let mut total = 0_usize;
        for value in self
            .args
            .iter()
            .chain(self.env.iter().flat_map(|(key, value)| [key, value]))
        {
            let length = value.as_bytes().len();
            if length > MAX_WORKER_METADATA_ITEM_BYTES {
                return Err(WorkerMetadataError::ItemTooLarge {
                    actual: length,
                    maximum: MAX_WORKER_METADATA_ITEM_BYTES,
                });
            }
            total = total.saturating_add(length);
        }
        if total > MAX_WORKER_METADATA_BYTES {
            return Err(WorkerMetadataError::MetadataTooLarge {
                actual: total,
                maximum: MAX_WORKER_METADATA_BYTES,
            });
        }
        if self.env.iter().any(|(key, _)| {
            key.as_bytes().is_empty()
                || key.as_bytes().contains(&0)
                || key.as_bytes().contains(&b'=')
        }) || self
            .env
            .iter()
            .any(|(_, value)| value.as_bytes().contains(&0))
            || self.args.iter().any(|value| value.as_bytes().contains(&0))
        {
            return Err(WorkerMetadataError::InvalidEnvironmentOrArgument);
        }
        Ok(())
    }
}

/// A launcher path and bounded ready-handshake wait policy.
#[derive(Clone, Debug)]
pub struct WorkerSpawner {
    launcher: PathBuf,
    handshake_timeout: Duration,
}

impl WorkerSpawner {
    /// Resolves the dedicated launcher and configures the ready-frame wait bound.
    ///
    /// `handshake_timeout` starts after the launcher has spawned and the challenge frame has been
    /// sent. It does not bound executable resolution, entropy reads, `spawn`, or the single bounded
    /// challenge send.
    ///
    /// # Errors
    ///
    /// Returns an error when the launcher cannot be resolved to an executable regular file.
    pub fn new(
        launcher: impl AsRef<OsStr>,
        handshake_timeout: Duration,
    ) -> Result<Self, ExecutableError> {
        Ok(Self {
            launcher: resolve_executable(launcher.as_ref())?,
            handshake_timeout,
        })
    }

    /// Spawns a dedicated process group and authenticates its direct worker process.
    ///
    /// Every failure after spawn sends `SIGKILL` to the process group and transfers direct-child
    /// reaping to a background reaper, so returning an error does not leave a zombie.
    ///
    /// # Errors
    ///
    /// Returns an error for entropy, socket, spawn, transport, timeout, early exit, or handshake
    /// authentication failures.
    pub fn spawn(
        &self,
        command: WorkerCommand,
        identity: WorkerIdentity,
    ) -> Result<WorkerProcess, SpawnError> {
        ensure_reaper()?;
        let nonce = generate_nonce()?;
        let (mut parent_endpoint, child_endpoint) = SeqpacketEndpoint::pair()?;
        let child = command
            .into_launcher_command(&self.launcher, child_endpoint)?
            .spawn()?;
        let pgid = child_pid(child.id())?;
        let mut starting = StartingChild::new(child, pgid);

        if let Err(error) = parent_endpoint.send(
            CHALLENGE,
            FrameFlags::default(),
            identity.instance,
            identity.generation,
            &authenticated_payload(identity.protocol, &nonce, &[]),
            &[],
        ) {
            if let Some(status) = wait_for_early_exit(&mut starting, self.handshake_timeout)? {
                return Err(SpawnError::EarlyExit(status));
            }
            return Err(SpawnError::Transport(error));
        }

        let frame =
            receive_startup_frame(&mut parent_endpoint, &mut starting, self.handshake_timeout)?;
        let worker_pid = verify_startup_ready(&frame, starting.id(), pgid, identity, &nonce)?;
        let worker_pid_u32 = u32::try_from(worker_pid).map_err(|_| SpawnError::InvalidChildPid)?;

        let child = starting.disarm();
        Ok(WorkerProcess {
            worker_pid: worker_pid_u32,
            expected_worker_pid: worker_pid,
            leader: LeaderState::new(child, pgid),
            endpoint: Some(parent_endpoint),
            identity,
            nonce,
        })
    }
}

/// Child-side authenticated supervision endpoint.
#[derive(Debug)]
pub struct WorkerEndpoint {
    endpoint: SeqpacketEndpoint,
    expected_parent_pid: i32,
    identity: WorkerIdentity,
    nonce: SpawnHandshakeNonce,
}

impl WorkerEndpoint {
    /// Adopts fd 0 and sends ready; this must be the worker's first process-level operation.
    ///
    /// The helper rejects repeated adoption and a process that already has multiple threads. It
    /// marks fd 0 `CLOEXEC` before duplicating it into owned `CLOEXEC` storage, then replaces fd 0
    /// with `/dev/null`. Applications must parse only the minimal non-secret mode metadata needed
    /// to construct `identity` before calling this function.
    ///
    /// # Errors
    ///
    /// Returns an error for late/repeated adoption, descriptor setup, transport, or challenge
    /// authentication failures.
    pub fn adopt_at_process_entry(identity: WorkerIdentity) -> Result<Self, ChildHandshakeError> {
        if STDIN_ADOPTED.swap(true, Ordering::AcqRel) {
            return Err(ChildHandshakeError::AlreadyAdopted);
        }
        if fs::read_dir("/proc/self/task")?.count() != 1 {
            return Err(ChildHandshakeError::MultipleThreads);
        }

        let stdin = io::stdin();
        let flags = fcntl_getfd(&stdin)?;
        fcntl_setfd(&stdin, flags | FdFlags::CLOEXEC)?;
        let owned = rustix::io::fcntl_dupfd_cloexec(&stdin, 0)?;
        let null = File::open(Path::new("/dev/null"))?;
        rustix::stdio::dup2_stdin(&null)?;

        let mut endpoint = SeqpacketEndpoint::from_owned_fd(owned)?;
        let challenge = endpoint.receive()?;
        if challenge.header().message_type() != CHALLENGE {
            return Err(ChildHandshakeError::UnexpectedMessage);
        }
        if challenge.header().instance() != identity.instance {
            return Err(ChildHandshakeError::InstanceMismatch);
        }
        if challenge.header().generation() != identity.generation {
            return Err(ChildHandshakeError::GenerationMismatch);
        }
        if !challenge.descriptors().is_empty() {
            return Err(ChildHandshakeError::UnexpectedDescriptors);
        }
        let (protocol, nonce, payload) = parse_authenticated_payload(challenge.payload())?;
        if protocol != identity.protocol {
            return Err(ChildHandshakeError::ProtocolMismatch {
                expected: identity.protocol,
                actual: protocol,
            });
        }
        if !payload.is_empty() {
            return Err(ChildHandshakeError::UnexpectedPayload);
        }
        let expected_parent_pid = challenge.peer_identity().pid();
        endpoint.send(
            READY,
            FrameFlags::default(),
            identity.instance,
            identity.generation,
            &authenticated_payload(protocol, &nonce, &[]),
            &[],
        )?;
        Ok(Self {
            endpoint,
            expected_parent_pid,
            identity,
            nonce,
        })
    }

    /// Sends one frame with the authenticated per-message prefix.
    ///
    /// # Errors
    ///
    /// Returns a transport error for bounds or operating-system failures.
    pub fn send(
        &mut self,
        message_type: MessageType,
        flags: FrameFlags,
        payload: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<Sequence, TransportError> {
        self.endpoint.send(
            message_type,
            flags,
            self.identity.instance,
            self.identity.generation,
            &authenticated_payload(self.identity.protocol, &self.nonce, payload),
            descriptors,
        )
    }

    /// Receives and authenticates one parent frame.
    ///
    /// # Errors
    ///
    /// Returns an error for transport failure or any credential/metadata mismatch.
    pub fn receive(&mut self) -> Result<AuthenticatedFrame, AuthenticatedChannelError> {
        let frame = self.endpoint.receive()?;
        authenticate_frame(frame, self.expected_parent_pid, self.identity, &self.nonce)
    }
}

/// A successfully authenticated worker process and its process group.
#[derive(Debug)]
pub struct WorkerProcess {
    worker_pid: u32,
    expected_worker_pid: i32,
    leader: LeaderState,
    endpoint: Option<SeqpacketEndpoint>,
    identity: WorkerIdentity,
    nonce: SpawnHandshakeNonce,
}

impl WorkerProcess {
    /// Returns the authenticated worker pid. The separate launcher owns the process group.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.worker_pid
    }

    /// Returns the currently pinned process-group id while its unreaped launcher leader is held.
    ///
    /// Once launcher status is observed, this returns `None` permanently and all group signaling
    /// methods become no-ops, preventing a reused numeric PGID from being targeted.
    #[must_use]
    pub fn process_group_id(&self) -> Option<u32> {
        self.leader
            .pgid
            .and_then(|pgid| u32::try_from(pgid.as_raw_pid()).ok())
    }

    /// Borrows the only parent-side channel interface.
    ///
    /// The channel authenticates every received frame and closes itself once direct-child exit is
    /// observed. It never exposes the unauthenticated seqpacket transport.
    #[must_use]
    pub fn channel(&mut self) -> AuthenticatedWorkerChannel<'_> {
        AuthenticatedWorkerChannel {
            endpoint: &mut self.endpoint,
            leader: &mut self.leader,
            expected_pid: self.expected_worker_pid,
            identity: self.identity,
            nonce: &self.nonce,
        }
    }

    /// Reports a newly observed launcher/process-group-leader exit exactly once.
    ///
    /// Reaping atomically invalidates the PGID. No later operation can signal that numeric group.
    /// The launcher normally kills its own group after reaping the actual worker, so its exit also
    /// indicates that ordinary in-group descendants received `SIGKILL`.
    ///
    /// # Errors
    ///
    /// Returns an operating-system wait error.
    pub fn poll_event(&mut self) -> io::Result<Option<WorkerEvent>> {
        let event = self.leader.poll_event()?;
        if self.leader.has_exited() {
            self.endpoint = None;
        }
        Ok(event)
    }

    /// Waits for and reports launcher exit, atomically invalidating the PGID as it reaps.
    ///
    /// # Errors
    ///
    /// Returns an operating-system wait error.
    pub fn wait_event(&mut self) -> io::Result<Option<WorkerEvent>> {
        let event = self.leader.wait_event()?;
        self.endpoint = None;
        Ok(event)
    }

    /// Sends `SIGTERM` to the process group, then `SIGKILL` if it outlives `grace`.
    ///
    /// The unreaped launcher identity pins the numeric PGID throughout both signals. The launcher
    /// is reaped only after `SIGKILL`, atomically disabling future group signals. This covers
    /// ordinary descendants in the group, not descendants that deliberately escape it.
    ///
    /// # Errors
    ///
    /// Returns an operating-system signal or wait error.
    pub fn terminate(&mut self, grace: Duration) -> io::Result<Option<WorkerEvent>> {
        self.leader.signal_group(Signal::TERM)?;
        thread::sleep(grace);
        self.leader.signal_group(Signal::KILL)?;
        self.wait_event()
    }

    /// Sends `SIGKILL` to the process group and reaps the direct child.
    ///
    /// This covers ordinary descendants in the worker's process group, not descendants that
    /// deliberately escape that group.
    ///
    /// # Errors
    ///
    /// Returns an operating-system signal or wait error.
    pub fn kill(&mut self) -> io::Result<Option<WorkerEvent>> {
        self.leader.signal_group(Signal::KILL)?;
        self.wait_event()
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.leader.signal_group(Signal::KILL);
        if let Some(child) = self.leader.take_child_and_invalidate_group() {
            submit_to_reaper(child);
        }
    }
}

/// Authenticated parent-side access to the worker channel.
pub struct AuthenticatedWorkerChannel<'a> {
    endpoint: &'a mut Option<SeqpacketEndpoint>,
    leader: &'a mut LeaderState,
    expected_pid: i32,
    identity: WorkerIdentity,
    nonce: &'a SpawnHandshakeNonce,
}

impl AuthenticatedWorkerChannel<'_> {
    /// Sends one authenticated frame, rejecting use after direct-child exit.
    ///
    /// # Errors
    ///
    /// Returns an error after child exit or for transport failures.
    pub fn send(
        &mut self,
        message_type: MessageType,
        flags: FrameFlags,
        payload: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<Sequence, AuthenticatedChannelError> {
        self.ensure_child_alive()?;
        self.endpoint
            .as_mut()
            .ok_or(AuthenticatedChannelError::ChannelClosed)?
            .send(
                message_type,
                flags,
                self.identity.instance,
                self.identity.generation,
                &authenticated_payload(self.identity.protocol, self.nonce, payload),
                descriptors,
            )
            .map_err(AuthenticatedChannelError::from)
    }

    /// Receives one frame while monitoring direct-child exit and authenticates every message.
    ///
    /// # Errors
    ///
    /// Returns an error after direct-child exit, transport failure, or any PID, nonce, instance,
    /// generation, or protocol mismatch. A rejected frame's descriptors are dropped.
    pub fn receive(&mut self) -> Result<AuthenticatedFrame, AuthenticatedChannelError> {
        loop {
            self.ensure_child_alive()?;
            let timeout = Timespec::try_from(CHILD_STATUS_POLL_INTERVAL).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "channel poll interval overflow",
                )
            })?;
            let ready = {
                let endpoint = self
                    .endpoint
                    .as_ref()
                    .ok_or(AuthenticatedChannelError::ChannelClosed)?;
                let mut descriptors = [PollFd::new(endpoint, PollFlags::IN)];
                poll(&mut descriptors, Some(&timeout)).map_err(io::Error::from)?
            };
            if ready == 0 {
                continue;
            }
            let result = self
                .endpoint
                .as_mut()
                .ok_or(AuthenticatedChannelError::ChannelClosed)?
                .receive();
            let frame = match result {
                Ok(frame) => frame,
                Err(error) => {
                    self.endpoint.take();
                    if let Some(status) = self.leader.refresh()? {
                        return Err(AuthenticatedChannelError::WorkerGroupExited(status));
                    }
                    if matches!(
                        error,
                        TransportError::Closed | TransportError::Io(rustix::io::Errno::CONNRESET)
                    ) {
                        let started = Instant::now();
                        while started.elapsed() < CHANNEL_CLOSE_EXIT_GRACE {
                            thread::sleep(REAP_POLL_INTERVAL);
                            if let Some(status) = self.leader.refresh()? {
                                return Err(AuthenticatedChannelError::WorkerGroupExited(status));
                            }
                        }
                    }
                    return Err(AuthenticatedChannelError::Transport(error));
                }
            };
            return authenticate_frame(frame, self.expected_pid, self.identity, self.nonce);
        }
    }

    fn ensure_child_alive(&mut self) -> Result<(), AuthenticatedChannelError> {
        if let Some(status) = self.leader.refresh()? {
            self.endpoint.take();
            return Err(AuthenticatedChannelError::WorkerGroupExited(status));
        }
        if self.endpoint.is_none() {
            return Err(AuthenticatedChannelError::ChannelClosed);
        }
        Ok(())
    }
}

/// One fully authenticated frame with its authentication prefix removed.
#[derive(Debug)]
pub struct AuthenticatedFrame {
    header: FrameHeader,
    payload: Vec<u8>,
    descriptors: Vec<OwnedFd>,
    peer_identity: PeerIdentity,
}

impl AuthenticatedFrame {
    /// Returns the validated transport header.
    #[must_use]
    pub const fn header(&self) -> FrameHeader {
        self.header
    }

    /// Returns the application payload after authentication metadata.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns descriptors received with this authenticated frame.
    #[must_use]
    pub fn descriptors(&self) -> &[OwnedFd] {
        &self.descriptors
    }

    /// Returns the kernel-authenticated direct peer identity.
    #[must_use]
    pub const fn peer_identity(&self) -> PeerIdentity {
        self.peer_identity
    }

    /// Separates frame metadata from descriptor ownership.
    #[must_use]
    pub fn into_parts(self) -> (FrameHeader, Vec<u8>, Vec<OwnedFd>, PeerIdentity) {
        (
            self.header,
            self.payload,
            self.descriptors,
            self.peer_identity,
        )
    }
}

/// Observable launcher/process-group lifecycle event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerEvent {
    /// The launcher/process-group leader exited, was reaped, and its PGID was invalidated.
    ///
    /// This is not the actual worker's exit status: on normal worker exit the launcher reaps that
    /// worker, kills all remaining in-group processes and itself, so this status is normally
    /// `SIGKILL`.
    ProcessGroupExited(ExitStatus),
}

/// Executable resolution failure before environment sanitization.
#[derive(Debug, Error)]
pub enum ExecutableError {
    /// No matching executable was found.
    #[error("executable could not be resolved: {program:?}")]
    NotFound { program: OsString },
    /// A candidate path could not be canonicalized.
    #[error("failed to resolve executable {path:?}: {source}")]
    Resolve { path: PathBuf, source: io::Error },
    /// The resolved path is not an executable regular file.
    #[error("resolved path is not an executable regular file: {path:?}")]
    NotExecutable { path: PathBuf },
}

/// Invalid bounded non-secret worker metadata.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorkerMetadataError {
    /// Too many worker arguments were configured.
    #[error("worker has {actual} arguments; maximum is {maximum}")]
    TooManyArguments { actual: usize, maximum: usize },
    /// Too many worker environment entries were configured.
    #[error("worker has {actual} environment entries; maximum is {maximum}")]
    TooManyEnvironmentEntries { actual: usize, maximum: usize },
    /// One metadata item exceeded its encoded bound.
    #[error("worker metadata item has {actual} bytes; maximum is {maximum}")]
    ItemTooLarge { actual: usize, maximum: usize },
    /// Aggregate metadata exceeded its encoded bound.
    #[error("worker metadata has {actual} bytes; maximum is {maximum}")]
    MetadataTooLarge { actual: usize, maximum: usize },
    /// An argument or environment entry cannot be represented by the OS APIs.
    #[error("worker argument or environment entry contains an invalid NUL or environment key")]
    InvalidEnvironmentOrArgument,
}

/// Parent-side spawn or startup authentication failure.
#[derive(Debug, Error)]
pub enum SpawnError {
    /// Operating-system I/O failure.
    #[error("worker process I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Supervision transport failure.
    #[error("worker handshake transport failed: {0}")]
    Transport(#[from] TransportError),
    /// Worker argv/environment metadata violated launcher bounds.
    #[error(transparent)]
    Metadata(#[from] WorkerMetadataError),
    /// Ready wait exceeded the configured handshake deadline.
    #[error("worker ready handshake timed out")]
    HandshakeTimeout,
    /// The direct child exited before authenticating startup.
    #[error("worker exited before ready: {0}")]
    EarlyExit(ExitStatus),
    /// The child pid could not be represented safely.
    #[error("child pid is outside the supported range")]
    InvalidChildPid,
    /// The authenticated worker was outside the launcher's pinned process group.
    #[error("ready credentials pid {worker_pid} was outside launcher process group {pgid}")]
    WorkerProcessGroupMismatch { worker_pid: i32, pgid: i32 },
    /// The authenticated sender was not the launcher's direct worker child.
    #[error("ready credentials pid {worker_pid} was not a direct child of launcher {launcher_pid}")]
    WorkerParentMismatch { worker_pid: i32, launcher_pid: u32 },
    /// The child response was not a ready frame.
    #[error("worker sent an unexpected handshake message")]
    UnexpectedMessage,
    /// The ready frame named a different instance.
    #[error("worker ready instance did not match")]
    InstanceMismatch,
    /// The ready frame named a different generation.
    #[error("worker ready generation {actual} did not match {expected}")]
    GenerationMismatch {
        expected: GenerationId,
        actual: GenerationId,
    },
    /// The ready frame named a different application protocol.
    #[error("worker ready protocol {actual} did not match {expected}")]
    ProtocolMismatch { expected: u16, actual: u16 },
    /// The ready frame did not echo the private nonce.
    #[error("worker ready nonce did not match")]
    NonceMismatch,
    /// A handshake frame unexpectedly transferred descriptors.
    #[error("worker handshake unexpectedly transferred descriptors")]
    UnexpectedDescriptors,
    /// A handshake payload had the wrong fixed shape.
    #[error("worker handshake payload is invalid")]
    InvalidPayload,
}

/// Child-side challenge validation failure.
#[derive(Debug, Error)]
pub enum ChildHandshakeError {
    /// Adoption was attempted more than once.
    #[error("worker stdin endpoint was already adopted")]
    AlreadyAdopted,
    /// Adoption occurred after additional threads were started.
    #[error("worker endpoint must be adopted before starting threads")]
    MultipleThreads,
    /// Operating-system I/O failure.
    #[error("worker stdin adoption failed: {0}")]
    Io(#[from] io::Error),
    /// Operating-system descriptor failure.
    #[error("worker stdin descriptor operation failed: {0}")]
    Descriptor(#[from] rustix::io::Errno),
    /// Supervision transport failure.
    #[error("worker handshake transport failed: {0}")]
    Transport(#[from] TransportError),
    /// The first parent frame was not a challenge.
    #[error("parent sent an unexpected handshake message")]
    UnexpectedMessage,
    /// The challenge named a different instance.
    #[error("parent challenge instance did not match")]
    InstanceMismatch,
    /// The challenge named a different generation.
    #[error("parent challenge generation did not match")]
    GenerationMismatch,
    /// The challenge named a different application protocol.
    #[error("parent challenge protocol {actual} did not match {expected}")]
    ProtocolMismatch { expected: u16, actual: u16 },
    /// A challenge unexpectedly transferred descriptors.
    #[error("parent challenge unexpectedly transferred descriptors")]
    UnexpectedDescriptors,
    /// The challenge carried an application payload.
    #[error("parent challenge carried an unexpected payload")]
    UnexpectedPayload,
    /// The challenge authentication prefix was malformed.
    #[error("parent challenge authentication payload is invalid")]
    InvalidPayload,
}

/// Per-message channel authentication failure.
#[derive(Debug, Error)]
pub enum AuthenticatedChannelError {
    /// Operating-system polling or child-status failure.
    #[error("authenticated channel I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Supervision transport failure.
    #[error("authenticated channel transport failed: {0}")]
    Transport(#[from] TransportError),
    /// Launcher/process-group exit closed the channel after worker disconnect.
    #[error("worker process group exited: {0}")]
    WorkerGroupExited(ExitStatus),
    /// The channel was already closed after direct-child exit.
    #[error("authenticated worker channel is closed")]
    ChannelClosed,
    /// Kernel credentials did not identify the direct child.
    #[error("frame credentials pid {actual} did not match direct child pid {expected}")]
    CredentialMismatch { expected: i32, actual: i32 },
    /// Frame instance metadata did not match.
    #[error("authenticated frame instance did not match")]
    InstanceMismatch,
    /// Frame generation metadata did not match.
    #[error("authenticated frame generation did not match")]
    GenerationMismatch,
    /// Frame protocol metadata did not match.
    #[error("authenticated frame protocol did not match")]
    ProtocolMismatch,
    /// Frame nonce did not match.
    #[error("authenticated frame nonce did not match")]
    NonceMismatch,
    /// Frame authentication prefix was malformed.
    #[error("authenticated frame prefix is invalid")]
    InvalidPayload,
}

#[derive(Debug)]
struct LeaderState {
    child: Option<Child>,
    pgid: Option<Pid>,
    status: Option<ExitStatus>,
    event_reported: bool,
}

impl LeaderState {
    fn new(child: Child, pgid: Pid) -> Self {
        Self {
            child: Some(child),
            pgid: Some(pgid),
            status: None,
            event_reported: false,
        }
    }

    fn refresh(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        if let Some(status) = child.try_wait()? {
            self.status = Some(status);
            self.child = None;
            self.pgid = None;
        }
        Ok(self.status)
    }

    fn poll_event(&mut self) -> io::Result<Option<WorkerEvent>> {
        let Some(status) = self.refresh()? else {
            return Ok(None);
        };
        if self.event_reported {
            return Ok(None);
        }
        self.event_reported = true;
        Ok(Some(WorkerEvent::ProcessGroupExited(status)))
    }

    fn wait_event(&mut self) -> io::Result<Option<WorkerEvent>> {
        if self.status.is_none() {
            if let Some(child) = self.child.as_mut() {
                self.status = Some(child.wait()?);
                self.child = None;
                self.pgid = None;
            }
        }
        self.poll_event()
    }

    const fn has_exited(&self) -> bool {
        self.status.is_some()
    }

    fn signal_group(&self, signal: Signal) -> io::Result<()> {
        match (&self.child, self.pgid) {
            (Some(_), Some(pgid)) => signal_group(pgid, signal),
            _ => Ok(()),
        }
    }

    fn take_child_and_invalidate_group(&mut self) -> Option<Child> {
        self.pgid = None;
        self.child.take()
    }
}

struct StartingChild {
    child: Option<Child>,
    pgid: Option<Pid>,
}

impl StartingChild {
    fn new(child: Child, pgid: Pid) -> Self {
        Self {
            child: Some(child),
            pgid: Some(pgid),
        }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("armed child guard").id()
    }

    fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        if let Some(status) = child.try_wait()? {
            self.child = None;
            self.pgid = None;
            return Ok(Some(status));
        }
        Ok(None)
    }

    fn disarm(mut self) -> Child {
        self.child.take().expect("armed child guard")
    }
}

impl Drop for StartingChild {
    fn drop(&mut self) {
        if let (Some(child), Some(pgid)) = (self.child.take(), self.pgid.take()) {
            let _ = signal_group(pgid, Signal::KILL);
            submit_to_reaper(child);
        }
    }
}

fn resolve_executable(program: &OsStr) -> Result<PathBuf, ExecutableError> {
    let requested = PathBuf::from(program);
    let candidate = if requested.is_absolute() || requested.components().count() > 1 {
        requested
    } else {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join(&requested))
            .find(|path| executable_file(path))
            .ok_or_else(|| ExecutableError::NotFound {
                program: program.to_owned(),
            })?
    };
    let resolved = fs::canonicalize(&candidate).map_err(|source| ExecutableError::Resolve {
        path: candidate,
        source,
    })?;
    if !executable_file(&resolved) {
        return Err(ExecutableError::NotExecutable { path: resolved });
    }
    Ok(resolved)
}

fn executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn encode_hex(bytes: &[u8]) -> OsString {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)]);
        encoded.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    OsString::from_vec(encoded)
}

fn generate_nonce() -> Result<SpawnHandshakeNonce, io::Error> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    File::open(Path::new("/dev/urandom"))?.read_exact(bytes.as_mut())?;
    Ok(SpawnHandshakeNonce::new(*bytes))
}

fn authenticated_payload(protocol: u16, nonce: &SpawnHandshakeNonce, payload: &[u8]) -> Vec<u8> {
    let mut authenticated = Vec::with_capacity(AUTH_PREFIX_SIZE + payload.len());
    authenticated.extend_from_slice(&protocol.to_be_bytes());
    authenticated.extend_from_slice(nonce.as_bytes());
    authenticated.extend_from_slice(payload);
    authenticated
}

fn parse_authenticated_payload(
    payload: &[u8],
) -> Result<(u16, SpawnHandshakeNonce, &[u8]), ChildHandshakeError> {
    if payload.len() < AUTH_PREFIX_SIZE {
        return Err(ChildHandshakeError::InvalidPayload);
    }
    Ok((
        u16::from_be_bytes(payload[..2].try_into().expect("fixed slice")),
        SpawnHandshakeNonce::new(
            payload[2..AUTH_PREFIX_SIZE]
                .try_into()
                .expect("fixed slice"),
        ),
        &payload[AUTH_PREFIX_SIZE..],
    ))
}

fn verify_startup_ready(
    frame: &Frame,
    launcher_pid: u32,
    pgid: Pid,
    identity: WorkerIdentity,
    nonce: &SpawnHandshakeNonce,
) -> Result<i32, SpawnError> {
    let worker_pid =
        Pid::from_raw(frame.peer_identity().pid()).ok_or(SpawnError::InvalidChildPid)?;
    if getpgid(Some(worker_pid)).map_err(io::Error::from)? != pgid {
        return Err(SpawnError::WorkerProcessGroupMismatch {
            worker_pid: worker_pid.as_raw_pid(),
            pgid: pgid.as_raw_pid(),
        });
    }
    if process_parent_pid(worker_pid)? != launcher_pid {
        return Err(SpawnError::WorkerParentMismatch {
            worker_pid: worker_pid.as_raw_pid(),
            launcher_pid,
        });
    }
    if frame.header().message_type() != READY {
        return Err(SpawnError::UnexpectedMessage);
    }
    if frame.header().instance() != identity.instance {
        return Err(SpawnError::InstanceMismatch);
    }
    if frame.header().generation() != identity.generation {
        return Err(SpawnError::GenerationMismatch {
            expected: identity.generation,
            actual: frame.header().generation(),
        });
    }
    if !frame.descriptors().is_empty() {
        return Err(SpawnError::UnexpectedDescriptors);
    }
    let Ok((protocol, received_nonce, payload)) = parse_authenticated_payload(frame.payload())
    else {
        return Err(SpawnError::InvalidPayload);
    };
    if protocol != identity.protocol {
        return Err(SpawnError::ProtocolMismatch {
            expected: identity.protocol,
            actual: protocol,
        });
    }
    if received_nonce.as_bytes() != nonce.as_bytes() {
        return Err(SpawnError::NonceMismatch);
    }
    if !payload.is_empty() {
        return Err(SpawnError::InvalidPayload);
    }
    Ok(worker_pid.as_raw_pid())
}

fn process_parent_pid(pid: Pid) -> io::Result<u32> {
    let status = fs::read_to_string(format!("/proc/{}/status", pid.as_raw_pid()))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:\t"))
        .and_then(|value| value.trim().parse::<u32>().ok())
        .ok_or_else(|| io::Error::other("worker process status omitted PPid"))
}

fn authenticate_frame(
    frame: Frame,
    expected_pid: i32,
    identity: WorkerIdentity,
    nonce: &SpawnHandshakeNonce,
) -> Result<AuthenticatedFrame, AuthenticatedChannelError> {
    let (header, payload, descriptors, peer_identity) = frame.into_parts();
    if peer_identity.pid() != expected_pid {
        return Err(AuthenticatedChannelError::CredentialMismatch {
            expected: expected_pid,
            actual: peer_identity.pid(),
        });
    }
    if header.instance() != identity.instance {
        return Err(AuthenticatedChannelError::InstanceMismatch);
    }
    if header.generation() != identity.generation {
        return Err(AuthenticatedChannelError::GenerationMismatch);
    }
    if payload.len() < AUTH_PREFIX_SIZE {
        return Err(AuthenticatedChannelError::InvalidPayload);
    }
    let protocol = u16::from_be_bytes(payload[..2].try_into().expect("fixed slice"));
    if protocol != identity.protocol {
        return Err(AuthenticatedChannelError::ProtocolMismatch);
    }
    if &payload[2..AUTH_PREFIX_SIZE] != nonce.as_bytes() {
        return Err(AuthenticatedChannelError::NonceMismatch);
    }
    Ok(AuthenticatedFrame {
        header,
        payload: payload[AUTH_PREFIX_SIZE..].to_vec(),
        descriptors,
        peer_identity,
    })
}

fn receive_startup_frame(
    endpoint: &mut SeqpacketEndpoint,
    child: &mut StartingChild,
    timeout: Duration,
) -> Result<Frame, SpawnError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.poll_exit()? {
            return Err(SpawnError::EarlyExit(status));
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(SpawnError::HandshakeTimeout);
        }
        let timespec = Timespec::try_from(remaining).map_err(|_| SpawnError::HandshakeTimeout)?;
        let mut descriptors = [PollFd::new(endpoint, PollFlags::IN)];
        match poll(&mut descriptors, Some(&timespec)) {
            Ok(0) => return Err(SpawnError::HandshakeTimeout),
            Ok(_) => match endpoint.receive() {
                Ok(frame) => return Ok(frame),
                Err(TransportError::Closed | TransportError::Io(rustix::io::Errno::CONNRESET)) => {
                    thread::sleep(REAP_POLL_INTERVAL);
                }
                Err(error) => return Err(SpawnError::Transport(error)),
            },
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(SpawnError::Io(error.into())),
        }
    }
}

fn wait_for_early_exit(
    child: &mut StartingChild,
    timeout: Duration,
) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.poll_exit()? {
            return Ok(Some(status));
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(None);
        }
        thread::sleep(REAP_POLL_INTERVAL.min(remaining));
    }
}

fn child_pid(pid: u32) -> Result<Pid, SpawnError> {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(SpawnError::InvalidChildPid)
}

fn signal_group(pgid: Pid, signal: Signal) -> io::Result<()> {
    match kill_process_group(pgid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct Reaper {
    sender: Option<Sender<Child>>,
    startup_error: Option<String>,
}

fn reaper() -> &'static Reaper {
    REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Child>();
        match thread::Builder::new()
            .name(String::from("oxiroute-worker-reaper"))
            .spawn(move || {
                let mut children = Vec::<Child>::new();
                loop {
                    match receiver.recv_timeout(REAP_POLL_INTERVAL) {
                        Ok(child) => children.push(child),
                        Err(mpsc::RecvTimeoutError::Disconnected) if children.is_empty() => break,
                        Err(
                            mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected,
                        ) => {}
                    }
                    while let Ok(child) = receiver.try_recv() {
                        children.push(child);
                    }
                    let mut index = 0;
                    while index < children.len() {
                        match children[index].try_wait() {
                            Ok(Some(_)) => {
                                let mut reaped = children.swap_remove(index);
                                let _ = reaped.wait();
                            }
                            Ok(None) | Err(_) => index += 1,
                        }
                    }
                }
            }) {
            Ok(_) => Reaper {
                sender: Some(sender),
                startup_error: None,
            },
            Err(error) => Reaper {
                sender: None,
                startup_error: Some(error.to_string()),
            },
        }
    })
}

fn ensure_reaper() -> io::Result<()> {
    match &reaper().startup_error {
        Some(error) => Err(io::Error::other(format!(
            "worker reaper thread failed to start: {error}"
        ))),
        None => Ok(()),
    }
}

fn submit_to_reaper(child: Child) {
    let Some(sender) = &reaper().sender else {
        // Worker creation calls ensure_reaper before spawning, so this is unreachable for an owned
        // child. Leaking is safer than Child::drop creating an immediately unreapable zombie.
        std::mem::forget(child);
        return;
    };
    if let Err(error) = sender.send(child) {
        // The dedicated receiver loop contains no panic path. If it nevertheless terminated, do
        // not make Drop unbounded; retain the OS child resource until process exit.
        std::mem::forget(error.0);
    }
}
