#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn master_config() -> Result<MasterConfig, Box<dyn Error>> {
    master_config_with_environment(|key: &str| std::env::var_os(key))
}

pub(super) fn master_config_with_environment(
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<MasterConfig, Box<dyn Error>> {
    Ok(MasterConfig::new(
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
        reload_drain_window(environment(RELOAD_DRAIN_ENV).as_deref())?,
        Duration::from_secs(6),
    )?)
}

/// Returns how long a replaced worker may keep serving the connections it already accepted.
///
/// The default outlasts the 2 hour read timeouts of long-lived streaming upstreams. The retired
/// worker exits as soon as its last connection ends, so the window only bounds the slowest one.
pub(super) fn reload_drain_window(value: Option<&std::ffi::OsStr>) -> Result<Duration, io::Error> {
    let Some(value) = value else {
        return Ok(DEFAULT_RELOAD_DRAIN);
    };
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{RELOAD_DRAIN_ENV} must be a whole number of seconds from 1 to {}",
                MAX_RELOAD_DRAIN.as_secs()
            ),
        )
    };
    let seconds = value
        .to_str()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .ok_or_else(invalid)?;
    let window = Duration::from_secs(seconds);
    if window.is_zero() || window > MAX_RELOAD_DRAIN {
        return Err(invalid());
    }
    Ok(window)
}

/// How long the worker retired by a replacement may keep serving its accepted connections: the
/// retiring generation's own `retirement` keys, then the deprecated environment variable, then
/// the default derived from its request deadlines.
pub(in crate::supervised) fn retirement_window(
    retiring: &ValidatedConfig,
    environment: Option<&std::ffi::OsStr>,
) -> Duration {
    let fallback = environment
        .and_then(|value| reload_drain_window(Some(value)).ok())
        .and_then(|window| u64::try_from(window.as_millis()).ok());
    Duration::from_millis(retiring.retirement_deadline_ms_with_default(fallback))
}

pub(super) fn worker_identity(
    token: InstanceToken,
) -> Result<(InstanceId, WorkerIdentity), Box<dyn Error>> {
    Ok((
        InstanceId::new(MASTER_INSTANCE_ID)?,
        WorkerIdentity {
            instance: token,
            generation: INITIAL_GENERATION,
            protocol: CONTROL_PROTOCOL_VERSION,
        },
    ))
}

pub(super) fn replacement_worker_identity(
    token: InstanceToken,
    generation: GenerationId,
) -> Result<(InstanceId, WorkerIdentity), Box<dyn Error>> {
    Ok((
        InstanceId::new(&format!("{MASTER_INSTANCE_ID}-{generation}"))?,
        WorkerIdentity {
            instance: token,
            generation,
            protocol: CONTROL_PROTOCOL_VERSION,
        },
    ))
}

pub(super) fn generate_instance_token() -> io::Result<InstanceToken> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(InstanceToken(bytes))
}

pub(super) fn encode_token(token: InstanceToken) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token.0 {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(super) fn shutdown_master(master: &mut Master) -> Result<(), Box<dyn Error>> {
    master.shutdown(Instant::now())?;
    loop {
        match master.state() {
            MasterState::Stopped => return Ok(()),
            MasterState::Failed => return Err(master_failure("shutdown").into()),
            _ => {
                master.poll(Instant::now())?;
                thread::sleep(MASTER_POLL_INTERVAL);
            }
        }
    }
}

pub(super) fn cleanup_master(master: &mut Master) {
    if matches!(master.state(), MasterState::Stopped | MasterState::Failed) {
        return;
    }
    let _ = master.shutdown(Instant::now());
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while !matches!(master.state(), MasterState::Stopped | MasterState::Failed)
        && Instant::now() < deadline
    {
        if master.poll(Instant::now()).is_err() {
            break;
        }
        thread::sleep(MASTER_POLL_INTERVAL);
    }
}

pub(super) fn master_failure(phase: &str) -> io::Error {
    io::Error::other(format!("supervised master failed during {phase}"))
}
