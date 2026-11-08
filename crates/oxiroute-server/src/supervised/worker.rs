use std::{
    error::Error,
    ffi::OsString,
    fmt::Display,
    io,
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::ValidatedConfig;
use oxiroute_server::{
    AdministrativeState, GenerationManager, GenerationStatus, ListenerRuntimeState,
    RuntimeGeneration, ServerAdminState, TrafficSnapshot, WorkerNotice,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision},
    worker_event_page,
};
use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, ControlRequest, MAX_STATUS_EVENTS,
    WorkerAdministrativeState, WorkerControl, WorkerEventRecord, WorkerGenerationStatus,
    WorkerLifecycle, WorkerListenerState, WorkerListenerStatus, WorkerMetrics, WorkerStatus,
};
use oxiroute_supervisor_process::WorkerIdentity;
use pingora::apps::AcceptGateClose;
use serde::{Deserialize, Serialize};

use self::status::{BUSY_TICK, StatusReporter};
use super::master::{RELOAD_DRAIN_ENV, retirement_window};
use crate::{
    GenerationProcess, finish_candidate_generation_process, shutdown_generation_processes,
};

const MAX_CONFIG_PATH_BYTES: usize = 4 * 1024;
const REJECT_INVALID_STATE: u8 = 1;
const REJECT_CONFIG: u8 = 2;
const REJECT_REVISION: u8 = 3;
const REJECT_RUNTIME: u8 = 4;
const REJECT_UNSUPPORTED: u8 = 5;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
// Keep worker lifecycle work below the master's ten-second quiesce/drain deadlines.
const LIFECYCLE_PHASE_TIMEOUT: Duration = Duration::from_secs(9);
// Startup waits for the generation's listeners by polling. The control loop does not poll: it sleeps on the
// control channel, so a request is served at once, and `StatusReporter` sets how long it sleeps between its
// own checks of the runtime, the drain and the status.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const TEST_RUNTIME_FAILURE_ENV: &str = "OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE";

#[allow(clippy::too_many_lines)]
pub(super) fn run(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let identity = parse_identity(&mut arguments)?;
    let mut control = WorkerControl::adopt_at_process_entry(identity)?;

    env_logger::init();
    let metadata = RuntimeMetadata::parse(&mut arguments)?;
    let mut adoption = control.receive()?;
    if adoption.phase() != ControlPhase::AdoptListeners {
        return Err(reject(
            &mut control,
            &adoption,
            REJECT_INVALID_STATE,
            "first worker control phase was not listener adoption".into(),
        ));
    }
    let Some(listeners) = adoption.take_listeners() else {
        return Err(reject(
            &mut control,
            &adoption,
            REJECT_INVALID_STATE,
            "listener adoption omitted its descriptor set".into(),
        ));
    };

    let coordinator = CanonicalConfigCoordinator::new(&metadata.config_path)?;
    let document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => {
            return Err(reject(
                &mut control,
                &adoption,
                REJECT_CONFIG,
                rejection.message().into(),
            ));
        }
    };
    if document.effective_revision != metadata.revision {
        return Err(reject(
            &mut control,
            &adoption,
            REJECT_REVISION,
            "canonical configuration revision did not match worker metadata".into(),
        ));
    }
    if let Err(error) = validate_stage_one_config(&document.validated_config) {
        return Err(reject(
            &mut control,
            &adoption,
            REJECT_UNSUPPORTED,
            error.into(),
        ));
    }
    let startup_deadline = Instant::now() + LIFECYCLE_PHASE_TIMEOUT;
    let manager = GenerationManager::new_supervised();
    let candidate =
        match manager.prepare_adopted_with_deadline(*document, listeners, startup_deadline) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Err(reject(&mut control, &adoption, REJECT_CONFIG, error.into()));
            }
        };
    let mut startup = match manager.begin_candidate_start(&candidate) {
        Ok(startup) => Some(startup),
        Err(error) => {
            return Err(reject(
                &mut control,
                &adoption,
                REJECT_RUNTIME,
                error.into(),
            ));
        }
    };
    let generation = match startup
        .as_mut()
        .expect("startup was just stored")
        .claim_runtime_start()
    {
        Ok(generation) => generation,
        Err(error) => {
            return Err(reject(
                &mut control,
                &adoption,
                REJECT_RUNTIME,
                error.into(),
            ));
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut quiesced = None;
    // The drain request stays open until every in-flight connection and stream has finished. At the
    // retirement deadline this worker closes its generation; the master's kill follows a backstop
    // later.
    let mut draining = None;
    let mut drain_ends = None;
    let mut forced_close = false;
    let mut process = match GenerationProcess::start(
        Arc::clone(&generation),
        coordinator,
        &manager,
        &stop,
        startup_deadline,
    ) {
        Ok(process) => Some(process),
        Err(error) => {
            return Err(reject(&mut control, &adoption, REJECT_RUNTIME, error));
        }
    };
    if let Err(error) = wait_for_generation_marker(
        &generation,
        process.as_ref().expect("generation process is owned"),
        startup_deadline,
    ) {
        let _ = finish_candidate_generation_process(
            &manager,
            process.take().expect("generation process is owned"),
            Instant::now() + SHUTDOWN_TIMEOUT,
        );
        drop(startup.take());
        return Err(reject(&mut control, &adoption, REJECT_RUNTIME, error));
    }
    let mut lifecycle = WorkerLifecycle::Ready;
    let mut reporter = StatusReporter::new(identity.generation, &generation, &manager);
    control.acknowledge(&adoption, ControlOutcome::Accepted)?;
    reporter.report(&mut control, lifecycle)?;

    let inject_runtime_failure = std::env::var_os(TEST_RUNTIME_FAILURE_ENV).is_some();
    loop {
        if generation.runtime_failed()
            || (!forced_close && process.as_ref().is_some_and(GenerationProcess::is_finished))
        {
            lifecycle = WorkerLifecycle::Failed;
            report_failure(&"generation runtime terminated unexpectedly");
            let _ = reporter.report(&mut control, lifecycle);
            drop(control);
            let process = process.take().expect("generation process is owned");
            let _ = finish_worker_generation_process(
                &manager,
                process,
                startup.is_some(),
                Instant::now() + SHUTDOWN_TIMEOUT,
            );
            drop(startup.take());
            return Err("generation runtime terminated unexpectedly".into());
        }

        let request = match control.try_receive() {
            Ok(Some(request)) => request,
            Ok(None) => {
                if draining.is_some()
                    && !forced_close
                    && drain_ends.is_some_and(|ends| Instant::now() >= ends)
                {
                    forced_close = true;
                    generation.announce_forced_close("drain_deadline");
                    process
                        .as_ref()
                        .expect("generation process is owned")
                        .request_shutdown();
                }
                let wait = if draining.is_some()
                    && (generation.drained()
                        || forced_close
                            && process.as_ref().is_some_and(GenerationProcess::is_finished))
                {
                    let request = draining.take().expect("a drain request is pending");
                    control.acknowledge(&request, ControlOutcome::Accepted)?;
                    reporter.report(&mut control, lifecycle)?;
                    BUSY_TICK
                } else {
                    reporter.report_when_due(&mut control, lifecycle)?
                };
                // A draining generation is polled for its last connection.
                let wait = if draining.is_some() {
                    wait.min(BUSY_TICK)
                } else {
                    wait
                };
                control.wait_readable(wait)?;
                continue;
            }
            Err(error) => {
                drop(control);
                let process = process.take().expect("generation process is owned");
                let _ = finish_worker_generation_process(
                    &manager,
                    process,
                    startup.is_some(),
                    Instant::now() + SHUTDOWN_TIMEOUT,
                );
                drop(startup.take());
                return Err(error.into());
            }
        };
        match request.phase() {
            ControlPhase::Activate if startup.is_some() => {
                let pending = startup.take().expect("activation is pending");
                let activation = HandOver::decode(request.attachment())
                    .map_err(Box::<dyn Error>::from)
                    .and_then(|previous| {
                        generation.restore_administrative_states(&previous.servers);
                        let replaced = previous.revision.map(|revision| revision.parse());
                        manager.set_supervised_lineage(
                            identity.generation.get(),
                            replaced.transpose()?,
                        );
                        manager.set_registry_writer(true);
                        pending.activate().map_err(Into::into)
                    });
                match activation {
                    Ok(_) => {
                        lifecycle = WorkerLifecycle::Active;
                        control.acknowledge(&request, ControlOutcome::Accepted)?;
                        reporter.report(&mut control, lifecycle)?;
                        if inject_runtime_failure {
                            generation.mark_runtime_failed();
                        }
                    }
                    Err(error) => {
                        manager.set_registry_writer(false);
                        lifecycle = WorkerLifecycle::Failed;
                        report_failure(&error);
                        control.acknowledge(&request, ControlOutcome::Rejected(REJECT_RUNTIME))?;
                        let _ = reporter.report(&mut control, lifecycle);
                        let _ = finish_candidate_generation_process(
                            &manager,
                            process.take().expect("generation process is owned"),
                            Instant::now() + SHUTDOWN_TIMEOUT,
                        );
                        return Err(error);
                    }
                }
            }
            ControlPhase::Shutdown => {
                lifecycle = WorkerLifecycle::Stopping;
                let process = process.take().expect("generation process is owned");
                let clean = finish_worker_generation_process(
                    &manager,
                    process,
                    startup.is_some(),
                    Instant::now() + SHUTDOWN_TIMEOUT,
                );
                drop(startup.take());
                let outcome = if clean {
                    ControlOutcome::Accepted
                } else {
                    ControlOutcome::Rejected(REJECT_RUNTIME)
                };
                control.acknowledge(&request, outcome)?;
                let _ = reporter.report(&mut control, lifecycle);
                return if clean {
                    Ok(())
                } else {
                    Err("generation shutdown did not complete cleanly".into())
                };
            }
            ControlPhase::Quiesce => {
                lifecycle = WorkerLifecycle::Quiescing;
                let outcome = quiesce_active(&manager, &mut quiesced);
                if matches!(outcome, ControlOutcome::Accepted) {
                    manager.set_registry_writer(false);
                }
                let outcome = if matches!(outcome, ControlOutcome::Accepted) {
                    hand_over(&mut control, &manager, &generation)
                } else {
                    outcome
                };
                control.acknowledge(&request, outcome)?;
                reporter.report(&mut control, lifecycle)?;
            }
            ControlPhase::Drain if draining.is_none() && start_drain(&manager) => {
                lifecycle = WorkerLifecycle::Draining;
                drain_ends = Some(
                    Instant::now()
                        + retirement_window(
                            generation.config(),
                            std::env::var_os(RELOAD_DRAIN_ENV).as_deref(),
                        ),
                );
                draining = Some(request);
                reporter.report(&mut control, lifecycle)?;
            }
            ControlPhase::Reactivate => {
                let outcome = reactivate_active(&mut quiesced);
                manager.set_registry_writer(matches!(outcome, ControlOutcome::Accepted));
                control.acknowledge(&request, outcome)?;
                lifecycle = if matches!(outcome, ControlOutcome::Accepted) {
                    WorkerLifecycle::Active
                } else {
                    WorkerLifecycle::Failed
                };
                reporter.report(&mut control, lifecycle)?;
            }
            ControlPhase::Notice => apply_notice(&manager, request.attachment())?,
            ControlPhase::AdoptListeners | ControlPhase::Activate | ControlPhase::Drain => {
                control.acknowledge(&request, ControlOutcome::Rejected(REJECT_INVALID_STATE))?;
                reporter.report(&mut control, lifecycle)?;
            }
        }
    }
}

/// Records what the master decided about the configuration file, or forgets it when accepted.
/// What the active worker passes to its replacement at activation.
#[derive(Default, Deserialize, Serialize)]
struct HandOver {
    revision: Option<String>,
    servers: Vec<ServerAdminState>,
}

impl HandOver {
    fn decode(attachment: &[u8]) -> Result<Self, serde_json::Error> {
        if attachment.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_slice(attachment)
    }
}

/// Reports the revision and runtime administrative state the next worker starts with. State too
/// large for the control channel refuses the replacement, so the old generation keeps serving
/// with its state.
fn hand_over(
    control: &mut WorkerControl,
    manager: &GenerationManager,
    generation: &RuntimeGeneration,
) -> ControlOutcome {
    let state = serde_json::to_vec(&HandOver {
        revision: manager
            .status()
            .active_revision
            .map(|revision| revision.to_string()),
        servers: generation.administrative_states(),
    })
    .expect("hand-over serializes");
    match control.report_state(&state) {
        Ok(_) => ControlOutcome::Accepted,
        Err(_) => ControlOutcome::Rejected(REJECT_RUNTIME),
    }
}

static FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

/// Prints why this worker is failing, once. The master kills a worker as soon as it rejects a phase or
/// reports itself failed, so the reason is printed before either.
pub(super) fn report_failure(error: &dyn Display) {
    if !FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
        eprintln!("OxiRoute supervised process failed: {error}");
    }
}

/// Rejects the startup request `request` with `code`, after reporting `error`.
fn reject(
    control: &mut WorkerControl,
    request: &ControlRequest,
    code: u8,
    error: Box<dyn Error>,
) -> Box<dyn Error> {
    report_failure(&error);
    match control.acknowledge(request, ControlOutcome::Rejected(code)) {
        Ok(_) => error,
        Err(acknowledgement) => acknowledgement.into(),
    }
}

fn apply_notice(manager: &GenerationManager, notice: &[u8]) -> Result<(), Box<dyn Error>> {
    match serde_json::from_slice::<WorkerNotice>(notice)? {
        WorkerNotice::Config {
            rejection: Some(rejection),
        } => {
            manager.reject_configuration(rejection);
        }
        WorkerNotice::Config { rejection: None } => manager.accept_configuration(),
        WorkerNotice::Retired { generations } => manager.set_remote_retired(generations)?,
    }
    Ok(())
}

fn finish_worker_generation_process(
    manager: &GenerationManager,
    process: GenerationProcess,
    unpublished: bool,
    deadline: Instant,
) -> bool {
    if unpublished {
        finish_candidate_generation_process(manager, process, deadline)
    } else {
        shutdown_generation_processes(manager, vec![process], deadline)
    }
}

fn parse_identity(
    arguments: &mut impl Iterator<Item = OsString>,
) -> Result<WorkerIdentity, Box<dyn Error>> {
    let generation = parse_ascii(arguments.next(), "generation", 20)?.parse::<u64>()?;
    let token = parse_token(arguments.next())?;
    Ok(WorkerIdentity {
        instance: InstanceToken(token),
        generation: GenerationId(generation),
        protocol: CONTROL_PROTOCOL_VERSION,
    })
}

struct RuntimeMetadata {
    config_path: PathBuf,
    revision: EffectiveRevision,
}

impl RuntimeMetadata {
    fn parse(arguments: &mut impl Iterator<Item = OsString>) -> Result<Self, Box<dyn Error>> {
        let config_path = arguments.next().ok_or("missing worker config path")?;
        if config_path.as_bytes().is_empty() || config_path.as_bytes().len() > MAX_CONFIG_PATH_BYTES
        {
            return Err("worker config path is empty or exceeds its bound".into());
        }
        let revision =
            parse_ascii(arguments.next(), "revision", 64)?.parse::<EffectiveRevision>()?;
        if arguments.next().is_some() {
            return Err("trailing worker metadata".into());
        }
        Ok(Self {
            config_path: PathBuf::from(config_path),
            revision,
        })
    }
}

pub(super) fn validate_stage_one_config(config: &ValidatedConfig) -> Result<(), &'static str> {
    oxiroute_server::ListenerReservations::validate_supervised_descriptor_limit(config)
}

fn wait_for_generation_marker(
    generation: &RuntimeGeneration,
    process: &GenerationProcess,
    deadline: Instant,
) -> Result<(), Box<dyn Error>> {
    let expected_listener_count = generation.expected_runtime_listener_count();
    loop {
        if generation.runtime_failed() || process.is_finished() {
            return Err(io::Error::other("generation listeners failed during startup").into());
        }
        let listeners = generation.metrics().internal_listener_snapshots()?;
        if generation.runtime_started()
            && listeners.len() == expected_listener_count
            && listeners
                .iter()
                .all(|listener| listener.state == oxiroute_server::ListenerRuntimeState::Listening)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "generation listeners did not become ready before their deadline",
            )
            .into());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn parse_ascii(
    value: Option<OsString>,
    label: &str,
    maximum: usize,
) -> Result<String, Box<dyn Error>> {
    let value = value.ok_or_else(|| format!("missing worker {label}"))?;
    if value.as_bytes().len() > maximum {
        return Err(format!("worker {label} exceeds its bound").into());
    }
    value
        .into_string()
        .map_err(|_| format!("worker {label} is not ASCII").into())
}

fn parse_token(value: Option<OsString>) -> Result<[u8; 16], Box<dyn Error>> {
    let value = value.ok_or("missing worker instance token")?;
    let bytes = value.as_bytes();
    if bytes.len() != 32 {
        return Err("worker instance token must contain 32 hexadecimal digits".into());
    }
    let mut token = [0_u8; 16];
    for (target, pair) in token.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *target = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Ok(token)
}

fn nibble(byte: u8) -> Result<u8, io::Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker instance token is not lowercase hexadecimal",
        )),
    }
}

fn quiesce_active(
    manager: &GenerationManager,
    quiesced: &mut Option<AcceptGateClose>,
) -> ControlOutcome {
    let Some(generation) = manager.active() else {
        return ControlOutcome::Rejected(REJECT_RUNTIME);
    };
    let close = generation.accept_gate().close();
    let complete = close.wait(LIFECYCLE_PHASE_TIMEOUT);
    *quiesced = Some(close);
    if complete {
        ControlOutcome::Accepted
    } else {
        ControlOutcome::Rejected(REJECT_RUNTIME)
    }
}

/// Stops the active generation from accepting and reports whether there is one to drain.
fn start_drain(manager: &GenerationManager) -> bool {
    manager.active().is_some_and(|generation| {
        generation.stop_accepting();
        generation.mark_retired();
        true
    })
}

fn reactivate_active(quiesced: &mut Option<AcceptGateClose>) -> ControlOutcome {
    if quiesced.take().is_some_and(AcceptGateClose::reopen) {
        ControlOutcome::Accepted
    } else {
        ControlOutcome::Rejected(REJECT_RUNTIME)
    }
}

mod status;

#[cfg(test)]
mod tests;
