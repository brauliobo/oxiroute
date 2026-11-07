#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn encode_hex(bytes: &[u8]) -> OsString {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)]);
        encoded.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    OsString::from_vec(encoded)
}

pub(super) fn generate_nonce() -> Result<SpawnHandshakeNonce, io::Error> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    File::open(Path::new("/dev/urandom"))?.read_exact(bytes.as_mut())?;
    Ok(SpawnHandshakeNonce::new(*bytes))
}

pub(super) fn authenticated_payload(
    protocol: u16,
    nonce: &SpawnHandshakeNonce,
    payload: &[u8],
) -> Vec<u8> {
    let mut authenticated = Vec::with_capacity(AUTH_PREFIX_SIZE + payload.len());
    authenticated.extend_from_slice(&protocol.to_be_bytes());
    authenticated.extend_from_slice(nonce.as_bytes());
    authenticated.extend_from_slice(payload);
    authenticated
}

pub(super) fn parse_authenticated_payload(
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

pub(super) fn verify_startup_ready(
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

pub(super) fn authenticate_frame(
    frame: Frame,
    expected_pid: i32,
    identity: WorkerIdentity,
    nonce: &SpawnHandshakeNonce,
) -> Result<AuthenticatedFrame, AuthenticatedChannelError> {
    if frame.peer_identity().pid() != expected_pid {
        return Err(AuthenticatedChannelError::CredentialMismatch {
            expected: expected_pid,
            actual: frame.peer_identity().pid(),
        });
    }
    if frame.header().instance() != identity.instance {
        return Err(AuthenticatedChannelError::InstanceMismatch);
    }
    if frame.header().generation() != identity.generation {
        return Err(AuthenticatedChannelError::GenerationMismatch);
    }
    let payload = frame.payload();
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
    Ok(AuthenticatedFrame { frame })
}

pub(super) fn receive_startup_frame(
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

pub(super) fn wait_for_early_exit(
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

pub(super) fn child_pid(pid: u32) -> Result<Pid, SpawnError> {
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(SpawnError::InvalidChildPid)
}

pub(super) fn signal_group(pgid: Pid, signal: Signal) -> io::Result<()> {
    match kill_process_group(pgid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
