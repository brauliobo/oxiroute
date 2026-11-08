#[allow(clippy::wildcard_imports)]
use super::*;

/// A launcher path and bounded ready-handshake wait policy.
#[derive(Clone, Debug)]
pub struct WorkerSpawner {
    launcher: PathBuf,
    handshake_timeout: Duration,
    cgroup_root: PathBuf,
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
            cgroup_root: PathBuf::from(DEFAULT_CGROUP_V2_ROOT),
        })
    }

    /// Overrides the delegated cgroup-v2 root used for worker containment.
    ///
    /// If the root does not expose a writable delegated hierarchy, spawning retains process-group
    /// containment without creating or writing any cgroup.
    #[must_use]
    pub fn with_cgroup_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.cgroup_root = root.into();
        self
    }

    /// Spawns a dedicated process group and authenticates its direct worker process.
    ///
    /// Every failure after spawn sends `SIGKILL` to the process group and transfers direct-child
    /// reaping to a background reaper, so returning an error does not leave a zombie.
    ///
    /// # Errors
    ///
    /// Returns an error for required containment, entropy, socket, spawn, transport, timeout,
    /// early exit, or handshake authentication failures.
    pub fn spawn(
        &self,
        command: WorkerCommand,
        identity: WorkerIdentity,
    ) -> Result<WorkerProcess, SpawnError> {
        ensure_reaper()?;
        let nonce = generate_nonce()?;
        let (mut parent_endpoint, child_endpoint) = SeqpacketEndpoint::pair()?;
        let require_cgroup_containment = command.require_cgroup_containment;
        let cgroup = match WorkerCgroupLease::try_create(&self.cgroup_root) {
            Ok(Some(cgroup)) => Some(cgroup),
            Ok(None) if require_cgroup_containment => {
                return Err(SpawnError::CgroupContainmentUnavailable(
                    cgroup::probe_cgroup_v2_at(&self.cgroup_root).status,
                ));
            }
            Err(_) if require_cgroup_containment => {
                return Err(SpawnError::CgroupContainmentSetupFailed);
            }
            Ok(None) | Err(_) => None,
        };
        let child = command
            .into_launcher_command(
                &self.launcher,
                child_endpoint,
                cgroup.as_ref().map(WorkerCgroupLease::path),
            )?
            .spawn()?;
        let pgid = child_pid(child.id())?;
        let mut starting = StartingChild::new(child, pgid, cgroup);

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

        let (child, cgroup) = starting.disarm();
        Ok(WorkerProcess {
            worker_pid: worker_pid_u32,
            expected_worker_pid: worker_pid,
            leader: LeaderState::new(child, pgid, cgroup),
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

    /// Blocks until the parent sent a frame or closed the channel, or `timeout` elapsed.
    ///
    /// Returns whether the channel is readable; it consumes nothing, so [`Self::try_receive`] follows.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout does not fit a poll timespec or polling fails.
    pub fn wait_readable(&self, timeout: Duration) -> io::Result<bool> {
        let timeout = Timespec::try_from(timeout)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "poll duration overflow"))?;
        let mut descriptors = [PollFd::new(&self.endpoint, PollFlags::IN)];
        Ok(poll(&mut descriptors, Some(&timeout)).map_err(io::Error::from)? > 0)
    }

    /// Receives and authenticates one parent frame only when the channel is immediately readable.
    ///
    /// # Errors
    ///
    /// Returns an error for polling, transport, or authentication failures.
    pub fn try_receive(&mut self) -> Result<Option<AuthenticatedFrame>, AuthenticatedChannelError> {
        let timeout = Timespec::try_from(Duration::ZERO).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "zero poll duration overflow")
        })?;
        let mut descriptors = [PollFd::new(&self.endpoint, PollFlags::IN)];
        if poll(&mut descriptors, Some(&timeout)).map_err(io::Error::from)? == 0 {
            return Ok(None);
        }
        self.receive().map(Some)
    }
}

pub(super) struct StartingChild {
    child: Option<Child>,
    pgid: Option<Pid>,
    cgroup: Option<WorkerCgroupLease>,
}

impl StartingChild {
    fn new(child: Child, pgid: Pid, cgroup: Option<WorkerCgroupLease>) -> Self {
        Self {
            child: Some(child),
            pgid: Some(pgid),
            cgroup,
        }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("armed child guard").id()
    }

    pub(super) fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        if let Some(status) = child.try_wait()? {
            self.child = None;
            self.pgid = None;
            if let Some(cgroup) = self.cgroup.take() {
                submit_cgroup_to_reaper(cgroup);
            }
            return Ok(Some(status));
        }
        Ok(None)
    }

    fn disarm(mut self) -> (Child, Option<WorkerCgroupLease>) {
        (
            self.child.take().expect("armed child guard"),
            self.cgroup.take(),
        )
    }
}

impl Drop for StartingChild {
    fn drop(&mut self) {
        if let (Some(child), Some(pgid)) = (self.child.take(), self.pgid.take()) {
            if let Some(cgroup) = self.cgroup.as_ref() {
                let _ = cgroup.kill();
            }
            let _ = signal_group(pgid, Signal::KILL);
            submit_to_reaper(child, self.cgroup.take());
        }
    }
}

pub(super) fn resolve_executable(program: &OsStr) -> Result<PathBuf, ExecutableError> {
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
