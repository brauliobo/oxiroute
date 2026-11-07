#[allow(clippy::wildcard_imports)]
use super::*;

/// A successfully authenticated worker process and its process group.
#[derive(Debug)]
pub struct WorkerProcess {
    pub(super) worker_pid: u32,
    pub(super) expected_worker_pid: i32,
    pub(super) leader: LeaderState,
    pub(super) endpoint: Option<SeqpacketEndpoint>,
    pub(super) identity: WorkerIdentity,
    pub(super) nonce: SpawnHandshakeNonce,
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

    /// Returns the owned worker cgroup path when delegated containment was acquired.
    #[must_use]
    pub fn cgroup_path(&self) -> Option<&Path> {
        self.leader.cgroup.as_ref().map(WorkerCgroupLease::path)
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

    /// Requests graceful process-group termination without waiting or reaping.
    ///
    /// The launcher identity remains pinned until [`Self::poll_event`] or [`Self::wait_event`]
    /// observes and reaps it. Repeated requests are harmless after exit.
    ///
    /// # Errors
    ///
    /// Returns an operating-system signal error.
    pub fn request_terminate(&mut self) -> io::Result<()> {
        self.leader.signal_group(Signal::TERM)
    }

    /// Requests forced process-group termination without waiting or reaping.
    ///
    /// The launcher identity remains pinned until [`Self::poll_event`] or [`Self::wait_event`]
    /// observes and reaps it. Repeated requests are harmless after exit.
    ///
    /// # Errors
    ///
    /// Returns an operating-system signal error.
    pub fn request_kill(&mut self) -> io::Result<()> {
        self.leader.signal_group(Signal::KILL)
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
        if let Some((child, cgroup)) = self.leader.take_child_and_invalidate_group() {
            submit_to_reaper(child, cgroup);
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

    /// Receives and authenticates one frame only when the channel is immediately readable.
    ///
    /// This method also checks direct-child status before polling. It is intended for explicit
    /// event loops that enforce their own deadlines rather than blocking in [`Self::receive`].
    ///
    /// # Errors
    ///
    /// Returns an error after child exit, for polling or transport failures, or for any PID, nonce,
    /// instance, generation, or protocol mismatch. A rejected frame's descriptors are dropped.
    pub fn try_receive(&mut self) -> Result<Option<AuthenticatedFrame>, AuthenticatedChannelError> {
        self.ensure_child_alive()?;
        let timeout = Timespec::try_from(Duration::ZERO).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "zero poll duration overflow")
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
            return Ok(None);
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
                return Err(AuthenticatedChannelError::Transport(error));
            }
        };
        authenticate_frame(frame, self.expected_pid, self.identity, self.nonce).map(Some)
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

/// Proof that one validated transport frame passed authentication.
#[derive(Debug)]
pub struct AuthenticatedFrame {
    pub(super) frame: Frame,
}

impl AuthenticatedFrame {
    /// Returns the validated transport header.
    #[must_use]
    pub const fn header(&self) -> FrameHeader {
        self.frame.header()
    }

    /// Returns the application payload after authentication metadata.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.frame.payload()[AUTH_PREFIX_SIZE..]
    }

    /// Returns descriptors received with this authenticated frame.
    #[must_use]
    pub fn descriptors(&self) -> &[OwnedFd] {
        self.frame.descriptors()
    }

    /// Returns the kernel-authenticated direct peer identity.
    #[must_use]
    pub const fn peer_identity(&self) -> PeerIdentity {
        self.frame.peer_identity()
    }

    /// Separates frame metadata from descriptor ownership.
    #[must_use]
    pub fn into_parts(self) -> (FrameHeader, Vec<u8>, Vec<OwnedFd>, PeerIdentity) {
        let (header, mut payload, descriptors, peer_identity) = self.frame.into_parts();
        let payload = payload.split_off(AUTH_PREFIX_SIZE);
        (header, payload, descriptors, peer_identity)
    }
}

#[derive(Debug)]
pub(super) struct LeaderState {
    child: Option<Child>,
    pgid: Option<Pid>,
    cgroup: Option<WorkerCgroupLease>,
    status: Option<ExitStatus>,
    event_reported: bool,
}

impl LeaderState {
    pub(super) fn new(child: Child, pgid: Pid, cgroup: Option<WorkerCgroupLease>) -> Self {
        Self {
            child: Some(child),
            pgid: Some(pgid),
            cgroup,
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
            self.release_cgroup();
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
        if self.status.is_none()
            && let Some(child) = self.child.as_mut()
        {
            self.status = Some(child.wait()?);
            self.child = None;
            self.pgid = None;
            self.release_cgroup();
        }
        self.poll_event()
    }

    const fn has_exited(&self) -> bool {
        self.status.is_some()
    }

    fn signal_group(&self, signal: Signal) -> io::Result<()> {
        let cgroup_result = if signal == Signal::KILL {
            self.cgroup.as_ref().map_or(Ok(()), WorkerCgroupLease::kill)
        } else {
            Ok(())
        };
        let group_result = match (&self.child, self.pgid) {
            (Some(_), Some(pgid)) => signal_group(pgid, signal),
            _ => Ok(()),
        };
        cgroup_result.and(group_result)
    }

    fn take_child_and_invalidate_group(&mut self) -> Option<(Child, Option<WorkerCgroupLease>)> {
        self.pgid = None;
        self.child.take().map(|child| (child, self.cgroup.take()))
    }

    fn release_cgroup(&mut self) {
        let Some(mut cgroup) = self.cgroup.take() else {
            return;
        };
        let _ = cgroup.kill();
        if !matches!(cgroup.cleanup(), Ok(true)) {
            submit_cgroup_to_reaper(cgroup);
        }
    }
}
