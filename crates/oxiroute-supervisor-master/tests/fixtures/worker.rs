use std::{
    env, fs, io,
    net::TcpListener,
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, ExitCode},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{DescriptorKind, DescriptorSet, InstanceToken, SlotId};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, WorkerAdministrativeState,
    WorkerControl, WorkerEventRecord, WorkerGenerationStatus, WorkerLifecycle, WorkerStatus,
};
use oxiroute_supervisor_process::WorkerIdentity;

#[allow(clippy::too_many_lines)]
fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let behavior = arguments.next().ok_or("missing behavior")?;
    if behavior == "probe-fd" {
        let target = arguments.next().ok_or("missing descriptor target")?;
        let leaked = fs::read_dir("/proc/self/fd")?.any(|entry| {
            entry
                .ok()
                .and_then(|entry| fs::read_link(entry.path()).ok())
                .is_some_and(|path| path.to_string_lossy() == target)
        });
        return Ok(if leaked {
            ExitCode::from(71)
        } else {
            ExitCode::SUCCESS
        });
    }
    if behavior == "containment-descendant-tree" {
        let pid_file = arguments.next().ok_or("missing nested pid file")?;
        let child = Command::new(env::current_exe()?)
            .arg("containment-descendant-sleep")
            .spawn()?;
        fs::write(pid_file, child.id().to_string())?;
        thread::sleep(Duration::from_secs(30));
        return Ok(ExitCode::SUCCESS);
    }
    if behavior == "containment-setsid-sleep" {
        rustix::process::setsid()?;
        thread::sleep(Duration::from_secs(30));
        return Ok(ExitCode::SUCCESS);
    }
    if behavior == "containment-descendant-sleep" {
        thread::sleep(Duration::from_secs(30));
        return Ok(ExitCode::SUCCESS);
    }
    let generation = arguments
        .next()
        .ok_or("missing generation")?
        .parse::<u64>()?;
    let token = decode_token(&arguments.next().ok_or("missing instance token")?)?;
    let descendant_pid_file = arguments.next();
    if arguments.next().is_some() {
        return Err("trailing fixture arguments".into());
    }
    let identity = WorkerIdentity {
        instance: InstanceToken(token),
        generation: GenerationId(generation),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let mut control = WorkerControl::adopt_at_process_entry(identity)?;
    if let Some(pid_file) = descendant_pid_file {
        spawn_containment_descendants(&pid_file)?;
    }
    let sync = Rendezvous::from_environment();
    let mut serving = None;
    let mut status_sequence = 1_u64;
    let mut notice = String::new();
    let mut activation_state = String::new();
    loop {
        let mut request = control.receive()?;
        let phase = request.phase();
        let name = format!("{phase:?}");
        sync.wait_released(generation, &name);
        if phase == ControlPhase::Notice {
            notice = String::from_utf8_lossy(request.attachment()).into_owned();
        }
        if behavior == reject_behavior(phase) {
            control.acknowledge(&request, ControlOutcome::Rejected(7))?;
            continue;
        }
        if behavior == "hold-activate" && phase == ControlPhase::Activate {
            continue;
        }
        if behavior == crash_behavior(phase) {
            std::process::abort();
        }
        if behavior == "disconnect-activate" && phase == ControlPhase::Activate {
            drop(control);
            thread::sleep(Duration::from_secs(30));
            return Ok(ExitCode::SUCCESS);
        }
        if phase == ControlPhase::AdoptListeners {
            let Some(listeners) = request.take_listeners() else {
                return Ok(ExitCode::from(70));
            };
            serving = Some(Serving::new(listeners, generation)?);
        }
        match phase {
            ControlPhase::Activate | ControlPhase::Reactivate => {
                serving
                    .as_ref()
                    .ok_or("listeners were not adopted")?
                    .activate();
            }
            ControlPhase::Quiesce | ControlPhase::Drain | ControlPhase::Shutdown => {
                serving
                    .as_ref()
                    .ok_or("listeners were not adopted")?
                    .quiesce();
            }
            ControlPhase::AdoptListeners | ControlPhase::Notice => {}
        }
        if behavior == "echo-state" && phase == ControlPhase::Quiesce {
            control.report_state(b"state-of-the-quiesced-worker")?;
        }
        if phase == ControlPhase::Activate {
            activation_state = String::from_utf8_lossy(request.attachment()).into_owned();
        }
        if behavior == "stale-adopt" && phase == ControlPhase::AdoptListeners {
            control.acknowledge_raw(
                request.request_id().saturating_sub(1),
                phase,
                ControlOutcome::Accepted,
            )?;
        }
        if behavior == "timeout-activate" && phase == ControlPhase::Activate {
            thread::sleep(Duration::from_secs(1));
        }
        if behavior == "timeout-adopt" && phase == ControlPhase::AdoptListeners {
            thread::sleep(Duration::from_secs(1));
        }
        if phase != ControlPhase::Notice {
            control.acknowledge(&request, ControlOutcome::Accepted)?;
        }
        control.report_status(&WorkerStatus {
            sequence: status_sequence,
            generation_id: identity.generation,
            lifecycle: match phase {
                ControlPhase::AdoptListeners => WorkerLifecycle::Ready,
                ControlPhase::Activate | ControlPhase::Reactivate | ControlPhase::Notice => {
                    WorkerLifecycle::Active
                }
                ControlPhase::Quiesce => WorkerLifecycle::Quiescing,
                ControlPhase::Drain => WorkerLifecycle::Draining,
                ControlPhase::Shutdown => WorkerLifecycle::Stopping,
            },
            administrative_state: if matches!(
                phase,
                ControlPhase::Quiesce | ControlPhase::Drain | ControlPhase::Shutdown
            ) {
                WorkerAdministrativeState::Drain
            } else {
                WorkerAdministrativeState::Ready
            },
            accepting: matches!(phase, ControlPhase::Activate | ControlPhase::Reactivate),
            runtime_started: true,
            runtime_failed: false,
            drained: matches!(phase, ControlPhase::Drain | ControlPhase::Shutdown),
            generation: WorkerGenerationStatus {
                disk_revision: None,
                candidate_revision: None,
                active_revision: None,
                previous_revision: None,
                quarantined_revision: None,
                active_accepting: matches!(
                    phase,
                    ControlPhase::Activate | ControlPhase::Reactivate
                ),
                degraded: false,
                last_failure: None,
                prepares: 0,
                activations: 0,
                failures: 0,
                rollbacks: 0,
            },
            metrics: None,
            listeners: Vec::new(),
            degraded: false,
            degradation: (behavior == "echo-state").then(|| format!("{activation_state}|{notice}")),
            event_cursor: u64::from(phase == ControlPhase::Activate),
            event_cursor_lost: false,
            events: if phase == ControlPhase::Activate {
                vec![WorkerEventRecord {
                    cursor: 1,
                    timestamp_unix_ms: None,
                    event: "generation_activate".into(),
                    outcome: "applied".into(),
                    revision: None,
                    certificate: None,
                    correlation_id: Some("fixture".into()),
                    source: Some("worker".into()),
                    operation: Some("generation_activate".into()),
                }]
            } else {
                Vec::new()
            },
        })?;
        status_sequence = status_sequence.saturating_add(1);
        sync.mark_done(generation, &name);
        if behavior == "malformed-status-after-activate" && phase == ControlPhase::Activate {
            control.report_status_raw(&[0, 1])?;
        }
        if behavior == "crash-after-quiesce" && phase == ControlPhase::Quiesce {
            sync.wait_released(generation, "Crash");
            std::process::abort();
        }
        if phase == ControlPhase::Shutdown && behavior != "ignore-shutdown" {
            sync.wait_released(generation, "Exit");
            if let Some(serving) = serving.take() {
                serving.stop();
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
}

/// How the test and the worker wait for each other, through files in a directory the test owns: the test holds a step
/// of a generation by creating `hold-<generation>-<step>`, and the worker waits while that file exists; the worker
/// creates `done-<generation>-<phase>` when it has sent the acknowledgement and the status of a request.
struct Rendezvous(Option<PathBuf>);

impl Rendezvous {
    /// The worker waits at most this long for a hold to be released, so that one forgotten by a failed test ends.
    const LONGEST_HOLD: Duration = Duration::from_mins(1);

    fn from_environment() -> Self {
        Self(env::var_os("OXIROUTE_INTERNAL_TEST_FIXTURE_SYNC").map(PathBuf::from))
    }

    fn wait_released(&self, generation: u64, step: &str) {
        let Some(directory) = &self.0 else { return };
        let hold = directory.join(format!("hold-{generation}-{step}"));
        let started = Instant::now();
        while hold.exists() && started.elapsed() < Self::LONGEST_HOLD {
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn mark_done(&self, generation: u64, phase: &str) {
        if let Some(directory) = &self.0 {
            let done = directory.join(format!("done-{generation}-{phase}"));
            // The directory is gone when the test that owned this worker has ended.
            if let Err(error) = fs::write(done, "") {
                assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
            }
        }
    }
}

fn spawn_containment_descendants(pid_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pid_file = std::path::PathBuf::from(pid_file);
    let nested_pid_file = pid_file.with_extension("nested.pid");
    let ordinary = Command::new(env::current_exe()?)
        .arg("containment-descendant-tree")
        .arg(&nested_pid_file)
        .spawn()?;
    let escaped = Command::new(env::current_exe()?)
        .arg("containment-setsid-sleep")
        .spawn()?;
    let nested = loop {
        if let Ok(pid) = fs::read_to_string(&nested_pid_file)
            && !pid.trim().is_empty()
        {
            break pid;
        }
        thread::sleep(Duration::from_millis(1));
    };
    fs::write(
        pid_file,
        format!("{}\n{}\n{}\n", ordinary.id(), nested.trim(), escaped.id()),
    )?;
    Ok(())
}

struct Serving {
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<io::Result<()>>>,
}

impl Serving {
    fn new(mut listeners: DescriptorSet, generation: u64) -> io::Result<Self> {
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let response = generation.to_be_bytes();
        let mut threads = Vec::new();
        for id in (1..=8).map(SlotId) {
            let Some(kind) = listeners.slot(id).map(|slot| slot.kind) else {
                continue;
            };
            let descriptor = listeners.take(id).map_err(io::Error::other)?;
            let (active, stop) = (Arc::clone(&active), Arc::clone(&stop));
            threads.push(match kind {
                DescriptorKind::TcpListener => {
                    let listener = TcpListener::from(descriptor);
                    thread::spawn(move || serve_tcp(&listener, &active, &stop, &response))
                }
                DescriptorKind::UnixListener => {
                    let listener = UnixListener::from(descriptor);
                    thread::spawn(move || serve_unix(&listener, &active, &stop, &response))
                }
                _ => return Err(io::Error::other("fixture serves stream listeners only")),
            });
        }
        Ok(Self {
            active,
            stop,
            threads,
        })
    }

    fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    fn quiesce(&self) {
        self.active.store(false, Ordering::Release);
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// Answers one client. A client that left before the answer, as a probe that gave up does, is not the worker's failure.
fn answer(mut stream: impl io::Write, response: &[u8]) -> io::Result<()> {
    match stream.write_all(response) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn serve_tcp(
    listener: &TcpListener,
    active: &AtomicBool,
    stop: &AtomicBool,
    response: &[u8],
) -> io::Result<()> {
    while !stop.load(Ordering::Acquire) {
        if active.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => answer(stream, response)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn serve_unix(
    listener: &UnixListener,
    active: &AtomicBool,
    stop: &AtomicBool,
    response: &[u8],
) -> io::Result<()> {
    while !stop.load(Ordering::Acquire) {
        if active.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => answer(stream, response)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn reject_behavior(phase: ControlPhase) -> &'static str {
    match phase {
        ControlPhase::AdoptListeners => "reject-adopt",
        ControlPhase::Quiesce => "reject-quiesce",
        ControlPhase::Activate => "reject-activate",
        ControlPhase::Drain => "reject-drain",
        ControlPhase::Reactivate => "reject-reactivate",
        ControlPhase::Shutdown => "reject-shutdown",
        ControlPhase::Notice => "reject-notice",
    }
}

fn crash_behavior(phase: ControlPhase) -> &'static str {
    match phase {
        ControlPhase::AdoptListeners => "crash-adopt",
        ControlPhase::Quiesce => "crash-quiesce",
        ControlPhase::Activate => "crash-activate",
        ControlPhase::Drain => "crash-drain",
        ControlPhase::Reactivate => "crash-reactivate",
        ControlPhase::Shutdown => "crash-shutdown",
        ControlPhase::Notice => "crash-notice",
    }
}

fn decode_token(encoded: &str) -> Result<[u8; 16], Box<dyn std::error::Error>> {
    if encoded.len() != 32 {
        return Err("instance token must contain 32 hexadecimal digits".into());
    }
    let mut token = [0_u8; 16];
    for (target, pair) in token.iter_mut().zip(encoded.as_bytes().as_chunks::<2>().0) {
        *target = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(token)
}

fn nibble(byte: u8) -> Result<u8, Box<dyn std::error::Error>> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("invalid hexadecimal token".into()),
    }
}
