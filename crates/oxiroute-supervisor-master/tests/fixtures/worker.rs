use std::{
    env, fs, io,
    io::Write as _,
    net::TcpListener,
    os::unix::net::UnixListener,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{DescriptorSet, InstanceToken, SlotId};
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
    let generation = arguments
        .next()
        .ok_or("missing generation")?
        .parse::<u64>()?;
    let token = decode_token(&arguments.next().ok_or("missing instance token")?)?;
    if arguments.next().is_some() {
        return Err("trailing fixture arguments".into());
    }
    let identity = WorkerIdentity {
        instance: InstanceToken(token),
        generation: GenerationId(generation),
        protocol: CONTROL_PROTOCOL_VERSION,
    };
    let mut control = WorkerControl::adopt_at_process_entry(identity)?;
    let mut serving = None;
    let mut status_sequence = 1_u64;
    loop {
        let mut request = control.receive()?;
        let phase = request.phase();
        if behavior == reject_behavior(phase) {
            control.acknowledge(&request, ControlOutcome::Rejected(7))?;
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
            ControlPhase::AdoptListeners => {}
        }
        if behavior == "stale-adopt" && phase == ControlPhase::AdoptListeners {
            control.acknowledge_raw(
                request.request_id().saturating_sub(1),
                phase,
                ControlOutcome::Accepted,
            )?;
        }
        if (behavior == "delay-activate" && phase == ControlPhase::Activate)
            || (behavior == "delay-quiesce" && phase == ControlPhase::Quiesce)
            || (behavior == "delay-reactivate" && phase == ControlPhase::Reactivate)
        {
            thread::sleep(Duration::from_millis(150));
        }
        control.acknowledge(&request, ControlOutcome::Accepted)?;
        control.report_status(&WorkerStatus {
            sequence: status_sequence,
            generation_id: identity.generation,
            lifecycle: match phase {
                ControlPhase::AdoptListeners => WorkerLifecycle::Ready,
                ControlPhase::Activate | ControlPhase::Reactivate => WorkerLifecycle::Active,
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
            degradation: None,
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
        if behavior == "malformed-status-after-activate" && phase == ControlPhase::Activate {
            control.report_status_raw(&[0, 1])?;
        }
        if behavior == "crash-after-quiesce" && phase == ControlPhase::Quiesce {
            thread::sleep(Duration::from_millis(50));
            std::process::abort();
        }
        if behavior == "crash-after-activate" && phase == ControlPhase::Activate {
            thread::sleep(Duration::from_millis(100));
            std::process::abort();
        }
        if phase == ControlPhase::Shutdown && behavior != "ignore-shutdown" {
            if let Some(serving) = serving.take() {
                serving.stop();
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
}

struct Serving {
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<io::Result<()>>>,
}

impl Serving {
    fn new(mut listeners: DescriptorSet, generation: u64) -> io::Result<Self> {
        let tcp = TcpListener::from(listeners.take(SlotId(1)).map_err(io::Error::other)?);
        let unix = UnixListener::from(listeners.take(SlotId(2)).map_err(io::Error::other)?);
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let response = generation.to_be_bytes();
        let tcp_active = Arc::clone(&active);
        let tcp_stop = Arc::clone(&stop);
        let tcp_thread = thread::spawn(move || serve_tcp(&tcp, &tcp_active, &tcp_stop, &response));
        let unix_active = Arc::clone(&active);
        let unix_stop = Arc::clone(&stop);
        let unix_thread =
            thread::spawn(move || serve_unix(&unix, &unix_active, &unix_stop, &response));
        Ok(Self {
            active,
            stop,
            threads: vec![tcp_thread, unix_thread],
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

fn serve_tcp(
    listener: &TcpListener,
    active: &AtomicBool,
    stop: &AtomicBool,
    response: &[u8],
) -> io::Result<()> {
    while !stop.load(Ordering::Acquire) {
        if active.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => stream.write_all(response)?,
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
                Ok((mut stream, _)) => stream.write_all(response)?,
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
    }
}

fn decode_token(encoded: &str) -> Result<[u8; 16], Box<dyn std::error::Error>> {
    if encoded.len() != 32 {
        return Err("instance token must contain 32 hexadecimal digits".into());
    }
    let mut token = [0_u8; 16];
    for (target, pair) in token.iter_mut().zip(encoded.as_bytes().chunks_exact(2)) {
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
