#[allow(clippy::wildcard_imports)]
use super::*;

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
    /// A worker requiring process-tree containment had no usable delegated cgroup-v2 root.
    #[error("required worker cgroup containment is unavailable ({0:?})")]
    CgroupContainmentUnavailable(CgroupV2ProbeStatus),
    /// A delegated per-worker cgroup could not be acquired before launch.
    #[error("required worker cgroup containment could not be established")]
    CgroupContainmentSetupFailed,
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
