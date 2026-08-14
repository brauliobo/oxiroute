use std::{
    error::Error,
    ffi::OsString,
    io,
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};

use oxiroute_config::ValidatedConfig;
use oxiroute_server::{
    AdministrativeState, GenerationManager, ListenerRuntimeState, RuntimeGeneration,
    RuntimeSnapshot,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, EffectiveRevision},
    worker_event_page,
};
use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, MAX_STATUS_EVENTS,
    WorkerAdministrativeState, WorkerControl, WorkerEventRecord, WorkerGenerationStatus,
    WorkerLifecycle, WorkerListenerState, WorkerListenerStatus, WorkerMetrics, WorkerStatus,
};
use oxiroute_supervisor_process::WorkerIdentity;
use pingora::apps::AcceptGateClose;

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
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const STATUS_INTERVAL: Duration = Duration::from_millis(250);
const TEST_RUNTIME_FAILURE_ENV: &str = "OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE";

#[allow(clippy::too_many_lines)]
pub(super) fn run(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let identity = parse_identity(&mut arguments)?;
    let mut control = WorkerControl::adopt_at_process_entry(identity)?;

    env_logger::init();
    let metadata = RuntimeMetadata::parse(&mut arguments)?;
    let mut adoption = control.receive()?;
    if adoption.phase() != ControlPhase::AdoptListeners {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_INVALID_STATE))?;
        return Err("first worker control phase was not listener adoption".into());
    }
    let Some(listeners) = adoption.take_listeners() else {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_INVALID_STATE))?;
        return Err("listener adoption omitted its descriptor set".into());
    };

    let coordinator = CanonicalConfigCoordinator::new(&metadata.config_path)?;
    let document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(_) => {
            control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_CONFIG))?;
            return Err("canonical configuration was rejected".into());
        }
    };
    if document.effective_revision != metadata.revision {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_REVISION))?;
        return Err("canonical configuration revision did not match worker metadata".into());
    }
    if let Err(error) = validate_stage_one_config(&document.validated_config) {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_UNSUPPORTED))?;
        return Err(error.into());
    }
    let startup_deadline = Instant::now() + LIFECYCLE_PHASE_TIMEOUT;
    let manager = GenerationManager::new_supervised();
    let candidate =
        match manager.prepare_adopted_with_deadline(*document, listeners, startup_deadline) {
            Ok(candidate) => candidate,
            Err(error) => {
                control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_CONFIG))?;
                return Err(error.into());
            }
        };
    let mut startup = match manager.begin_candidate_start(&candidate) {
        Ok(startup) => Some(startup),
        Err(error) => {
            control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_RUNTIME))?;
            return Err(error.into());
        }
    };
    let generation = match startup
        .as_mut()
        .expect("startup was just stored")
        .claim_runtime_start()
    {
        Ok(generation) => generation,
        Err(error) => {
            control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_RUNTIME))?;
            return Err(error.into());
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut quiesced = None;
    let mut process = match GenerationProcess::start(
        Arc::clone(&generation),
        coordinator,
        &manager,
        &stop,
        startup_deadline,
    ) {
        Ok(process) => Some(process),
        Err(error) => {
            control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_RUNTIME))?;
            return Err(error);
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
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_RUNTIME))?;
        return Err(error);
    }
    let mut lifecycle = WorkerLifecycle::Ready;
    let mut status_sequence = 1_u64;
    let mut event_cursor = 0_u64;
    control.acknowledge(&adoption, ControlOutcome::Accepted)?;
    report_status(
        &mut control,
        &mut status_sequence,
        &mut event_cursor,
        identity.generation,
        &generation,
        &manager,
        lifecycle,
    )?;
    let mut last_status = Instant::now();

    let inject_runtime_failure = std::env::var_os(TEST_RUNTIME_FAILURE_ENV).is_some();
    loop {
        if generation.runtime_failed()
            || process.as_ref().is_some_and(GenerationProcess::is_finished)
        {
            lifecycle = WorkerLifecycle::Failed;
            let _ = report_status(
                &mut control,
                &mut status_sequence,
                &mut event_cursor,
                identity.generation,
                &generation,
                &manager,
                lifecycle,
            );
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
                if last_status.elapsed() >= STATUS_INTERVAL {
                    report_status(
                        &mut control,
                        &mut status_sequence,
                        &mut event_cursor,
                        identity.generation,
                        &generation,
                        &manager,
                        lifecycle,
                    )?;
                    last_status = Instant::now();
                }
                thread::sleep(POLL_INTERVAL);
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
                let activation = startup.take().expect("activation is pending").activate();
                match activation {
                    Ok(_) => {
                        lifecycle = WorkerLifecycle::Active;
                        control.acknowledge(&request, ControlOutcome::Accepted)?;
                        report_status(
                            &mut control,
                            &mut status_sequence,
                            &mut event_cursor,
                            identity.generation,
                            &generation,
                            &manager,
                            lifecycle,
                        )?;
                        last_status = Instant::now();
                        if inject_runtime_failure {
                            generation.mark_runtime_failed();
                        }
                    }
                    Err(error) => {
                        lifecycle = WorkerLifecycle::Failed;
                        control.acknowledge(&request, ControlOutcome::Rejected(REJECT_RUNTIME))?;
                        let _ = report_status(
                            &mut control,
                            &mut status_sequence,
                            &mut event_cursor,
                            identity.generation,
                            &generation,
                            &manager,
                            lifecycle,
                        );
                        let _ = finish_candidate_generation_process(
                            &manager,
                            process.take().expect("generation process is owned"),
                            Instant::now() + SHUTDOWN_TIMEOUT,
                        );
                        return Err(error.into());
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
                let _ = report_status(
                    &mut control,
                    &mut status_sequence,
                    &mut event_cursor,
                    identity.generation,
                    &generation,
                    &manager,
                    lifecycle,
                );
                return if clean {
                    Ok(())
                } else {
                    Err("generation shutdown did not complete cleanly".into())
                };
            }
            ControlPhase::Quiesce => {
                lifecycle = WorkerLifecycle::Quiescing;
                let outcome = quiesce_active(&manager, &mut quiesced);
                control.acknowledge(&request, outcome)?;
                report_status(
                    &mut control,
                    &mut status_sequence,
                    &mut event_cursor,
                    identity.generation,
                    &generation,
                    &manager,
                    lifecycle,
                )?;
            }
            ControlPhase::Drain => {
                lifecycle = WorkerLifecycle::Draining;
                let outcome = drain_active(&manager);
                control.acknowledge(&request, outcome)?;
                report_status(
                    &mut control,
                    &mut status_sequence,
                    &mut event_cursor,
                    identity.generation,
                    &generation,
                    &manager,
                    lifecycle,
                )?;
            }
            ControlPhase::Reactivate => {
                let outcome = reactivate_active(&mut quiesced);
                control.acknowledge(&request, outcome)?;
                lifecycle = if matches!(outcome, ControlOutcome::Accepted) {
                    WorkerLifecycle::Active
                } else {
                    WorkerLifecycle::Failed
                };
                report_status(
                    &mut control,
                    &mut status_sequence,
                    &mut event_cursor,
                    identity.generation,
                    &generation,
                    &manager,
                    lifecycle,
                )?;
            }
            ControlPhase::AdoptListeners | ControlPhase::Activate => {
                control.acknowledge(&request, ControlOutcome::Rejected(REJECT_INVALID_STATE))?;
                report_status(
                    &mut control,
                    &mut status_sequence,
                    &mut event_cursor,
                    identity.generation,
                    &generation,
                    &manager,
                    lifecycle,
                )?;
            }
        }
    }
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

#[allow(clippy::too_many_lines)]
fn report_status(
    control: &mut WorkerControl,
    sequence: &mut u64,
    event_cursor: &mut u64,
    generation_id: GenerationId,
    generation: &RuntimeGeneration,
    manager: &GenerationManager,
    lifecycle: WorkerLifecycle,
) -> Result<(), Box<dyn Error>> {
    let generation_status = manager.status();
    let runtime_snapshot = generation.metrics().snapshot();
    let internal_listeners = generation.metrics().internal_listener_snapshots();
    let metrics = runtime_snapshot.as_ref().ok().map(metrics_status);
    let listeners = internal_listeners
        .as_ref()
        .map_or_else(|_| Vec::new(), |listeners| listener_statuses(listeners));

    let event_page = worker_event_page(*event_cursor, MAX_STATUS_EVENTS);
    let events = event_page
        .events
        .iter()
        .map(|event| WorkerEventRecord {
            cursor: event.cursor,
            timestamp_unix_ms: event.timestamp_unix_ms,
            event: event.event.as_str().to_owned(),
            outcome: event.outcome.as_str().to_owned(),
            revision: event.revision.as_ref().map(ToString::to_string),
            certificate: event.certificate.clone(),
            correlation_id: event.correlation_id.clone(),
            source: event.source.clone(),
            operation: event.operation.clone(),
        })
        .collect::<Vec<_>>();
    let next_event_cursor = if event_page.has_more {
        event_page
            .events
            .last()
            .map_or(*event_cursor, |event| event.cursor)
    } else {
        event_page.latest_cursor
    };
    let metrics_degraded = runtime_snapshot.is_err() || internal_listeners.is_err();
    let degraded = generation_status.degraded
        || generation.runtime_failed()
        || metrics_degraded
        || event_page.cursor_lost;
    let degradation = if metrics_degraded {
        Some("metrics_unavailable".to_owned())
    } else if event_page.cursor_lost {
        Some("event_cursor_lost".to_owned())
    } else if generation_status.degraded {
        generation_status.last_failure.map_or_else(
            || Some("generation_degraded".to_owned()),
            |value| Some(value.into()),
        )
    } else if generation.runtime_failed() {
        Some("runtime_failed".to_owned())
    } else {
        None
    };
    let process_administrative_state = runtime_snapshot.as_ref().map_or_else(
        |_| match lifecycle {
            WorkerLifecycle::Quiescing | WorkerLifecycle::Draining | WorkerLifecycle::Stopping => {
                WorkerAdministrativeState::Drain
            }
            _ => WorkerAdministrativeState::Ready,
        },
        |snapshot| administrative_state(snapshot.process.administrative_state),
    );
    let status = WorkerStatus {
        sequence: *sequence,
        generation_id,
        lifecycle,
        administrative_state: process_administrative_state,
        accepting: generation.accepting(),
        runtime_started: generation.runtime_started(),
        runtime_failed: generation.runtime_failed(),
        drained: generation.drained(),
        generation: WorkerGenerationStatus {
            disk_revision: generation_status
                .disk_revision
                .as_ref()
                .map(ToString::to_string),
            candidate_revision: generation_status
                .candidate_revision
                .as_ref()
                .map(ToString::to_string),
            active_revision: generation_status
                .active_revision
                .as_ref()
                .map(ToString::to_string),
            previous_revision: generation_status
                .previous_revision
                .as_ref()
                .map(ToString::to_string),
            quarantined_revision: generation_status
                .quarantined_revision
                .as_ref()
                .map(ToString::to_string),
            active_accepting: generation_status.active_accepting,
            degraded: generation_status.degraded,
            last_failure: generation_status.last_failure.map(str::to_owned),
            prepares: generation_status.prepares,
            activations: generation_status.activations,
            failures: generation_status.failures,
            rollbacks: generation_status.rollbacks,
        },
        metrics,
        listeners,
        degraded,
        degradation,
        event_cursor: next_event_cursor,
        event_cursor_lost: event_page.cursor_lost,
        events,
    };
    control.report_status(&status)?;
    *sequence = (*sequence)
        .checked_add(1)
        .ok_or_else(|| io::Error::other("worker status sequence exhausted"))?;
    *event_cursor = next_event_cursor;
    Ok(())
}

fn metrics_status(snapshot: &RuntimeSnapshot) -> WorkerMetrics {
    WorkerMetrics {
        accepted_connections: snapshot.traffic.accepted_connections,
        rejected_connections: snapshot.traffic.rejected_connections,
        active_connections: snapshot.traffic.active_connections,
        bytes_received: snapshot.traffic.bytes_received,
        bytes_sent: snapshot.traffic.bytes_sent,
    }
}

fn listener_statuses(listeners: &[oxiroute_server::ListenerSnapshot]) -> Vec<WorkerListenerStatus> {
    listeners
        .iter()
        .map(|listener| WorkerListenerStatus {
            name: listener.name.clone(),
            protocol: listener.protocol.clone(),
            bind: listener.bind.clone(),
            administrative_state: administrative_state(listener.administrative_state),
            state: listener_state(listener.state),
            accepted_connections: listener.accepted_connections,
            rejected_connections: listener.rejected_connections,
            active_connections: listener.active_connections,
            bytes_received: listener.bytes_received,
            bytes_sent: listener.bytes_sent,
        })
        .collect()
}

const fn listener_state(state: ListenerRuntimeState) -> WorkerListenerState {
    match state {
        ListenerRuntimeState::Configured => WorkerListenerState::Configured,
        ListenerRuntimeState::Listening => WorkerListenerState::Listening,
        ListenerRuntimeState::Stopped => WorkerListenerState::Stopped,
        ListenerRuntimeState::Failed => WorkerListenerState::Failed,
    }
}

const fn administrative_state(state: AdministrativeState) -> WorkerAdministrativeState {
    match state {
        AdministrativeState::Ready => WorkerAdministrativeState::Ready,
        AdministrativeState::Drain => WorkerAdministrativeState::Drain,
        AdministrativeState::Maintenance => WorkerAdministrativeState::Maintenance,
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
    for (target, pair) in token.iter_mut().zip(bytes.chunks_exact(2)) {
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

fn drain_active(manager: &GenerationManager) -> ControlOutcome {
    let Some(generation) = manager.active() else {
        return ControlOutcome::Rejected(REJECT_RUNTIME);
    };
    if generation.drain(LIFECYCLE_PHASE_TIMEOUT) {
        ControlOutcome::Accepted
    } else {
        ControlOutcome::Rejected(REJECT_RUNTIME)
    }
}

fn reactivate_active(quiesced: &mut Option<AcceptGateClose>) -> ControlOutcome {
    if quiesced.take().is_some_and(AcceptGateClose::reopen) {
        ControlOutcome::Accepted
    } else {
        ControlOutcome::Rejected(REJECT_RUNTIME)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use oxiroute_config::{ConfigDraft, Protocol, Stats};
    use oxiroute_config_source::{ConfigFormat, render_config};
    use serde_json::json;

    use super::*;

    #[test]
    fn worker_listener_statuses_are_exact_real_snapshots_without_synthesis() {
        let snapshots = vec![oxiroute_server::ListenerSnapshot {
            administrative_state: AdministrativeState::Ready,
            name: "@management".into(),
            protocol: "http".into(),
            bind: "socket:127.0.0.1:9900".into(),
            max_connections: None,
            state: ListenerRuntimeState::Listening,
            accepted_connections: 3,
            rejected_connections: 1,
            active_connections: 2,
            bytes_received: 11,
            bytes_sent: 13,
            http_operations: None,
            tcp_relays: None,
            proxy_protocol: None,
            cache: None,
        }];

        let statuses = listener_statuses(&snapshots);

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].name, "@management");
        assert_eq!(statuses[0].state, WorkerListenerState::Listening);
        assert_eq!(statuses[0].accepted_connections, 3);
        assert_eq!(statuses[0].rejected_connections, 1);
        assert_eq!(statuses[0].active_connections, 2);
        assert_eq!(statuses[0].bytes_received, 11);
        assert_eq!(statuses[0].bytes_sent, 13);
    }

    #[test]
    fn deferred_lifecycle_phases_are_supported() {
        let manager = GenerationManager::new();
        let mut quiesced = None;

        assert!(matches!(
            quiesce_active(&manager, &mut quiesced),
            ControlOutcome::Rejected(REJECT_RUNTIME)
        ));
        assert!(matches!(
            drain_active(&manager),
            ControlOutcome::Rejected(REJECT_RUNTIME)
        ));
        assert!(matches!(
            reactivate_active(&mut quiesced),
            ControlOutcome::Rejected(REJECT_RUNTIME)
        ));
    }

    #[test]
    fn prepublication_runtime_failure_retires_recording_authority_before_quarantine() {
        assert_prepublication_recording_cleanup("runtime_failure");
    }

    #[test]
    fn prepublication_control_error_retires_recording_authority_before_quarantine() {
        assert_prepublication_recording_cleanup("control_error");
    }

    #[test]
    fn prepublication_shutdown_retires_recording_authority_before_quarantine() {
        assert_prepublication_recording_cleanup("shutdown");
    }

    fn assert_prepublication_recording_cleanup(branch: &str) {
        let directory = tempfile::tempdir().expect("supervised recording directory");
        let root = directory.path().join("recordings");
        std::fs::create_dir(&root).expect("recording root");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("RTMP listener probe");
        let listener_address = listener.local_addr().expect("RTMP listener address");
        drop(listener);
        let config: ConfigDraft = serde_json::from_value(json!({
            "version": 1,
            "listeners": [{
                "name": "ingest",
                "bind": {"type": "socket", "address": listener_address},
                "protocol": "rtmp",
                "service": "live"
            }],
            "rtmp_services": [{
                "name": "live",
                "applications": [{
                    "name": "live",
                    "live": true,
                    "recorders": [{"name": "archive", "root_directory": root}]
                }]
            }]
        }))
        .expect("recording config");
        let path = directory.path().join("oxiroute.kdl");
        std::fs::write(
            &path,
            render_config(
                ConfigFormat::Kdl,
                &config.clone().validate().expect("valid recording config"),
            )
            .expect("render recording config"),
        )
        .expect("write recording config");
        let coordinator = CanonicalConfigCoordinator::new(&path).expect("coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("recording config rejected")
        };
        let manager = GenerationManager::new();
        let candidate = manager.prepare(*document).expect("candidate");
        let mut startup = Some(
            manager
                .begin_candidate_start(&candidate)
                .expect("startup reservation"),
        );
        let generation = startup
            .as_mut()
            .expect("startup")
            .claim_runtime_start()
            .expect("runtime start");
        let detached_runtime = generation.rtmp_service("live").expect("RTMP runtime");
        let (release, released) = mpsc::sync_channel(0);
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            released.recv().expect("release detached worker");
            drop(detached_runtime.session());
            finished.send(()).expect("detached completion receiver");
        });
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let process = GenerationProcess {
            retirement: crate::CandidateRtmpRetirementHandle::capture(&generation),
            generation,
            shutdown,
            thread,
        };
        let started = Instant::now();

        assert!(finish_worker_generation_process(
            &manager,
            process,
            true,
            started + Duration::from_millis(75),
        ));
        drop(startup.take());

        assert!(started.elapsed() < Duration::from_millis(250), "{branch}");
        assert_eq!(manager.status().failures, 1, "{branch}");
        let validated = config.clone().validate().expect("retry config");
        let service = oxiroute_server::service_specs(&validated)
            .expect("retry service plans")
            .into_iter()
            .find_map(|service| match service.kind {
                oxiroute_server::ServiceKind::Rtmp(service) => Some(service.value_plan()),
                _ => None,
            })
            .expect("RTMP retry plan");
        drop(
            oxiroute_rtmp::PreparedRtmpRuntimeSet::prepare(
                [service],
                &oxiroute_rtmp::RtmpPrepareContext::new(
                    oxiroute_rtmp::RtmpPrepareMode::Activation,
                    [],
                ),
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_or_else(|error| panic!("{branch} recording retry failed: {error}")),
        );
        release.send(()).expect("release detached worker");
        completion
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|error| panic!("{branch} detached exit failed: {error}"));
    }

    #[test]
    fn stage_two_configuration_accepts_stream_listeners_and_statistics() {
        let mut config: ConfigDraft = serde_json::from_value(json!({
            "version": 1,
            "listeners": [],
            "http_services": [{
                "name": "http",
                "routes": [{
                    "path": {"kind": "segment_prefix", "value": "/"},
                    "action": {"type": "fixed_response", "status": 200}
                }]
            }],
            "upstream_pools": [{
                "name": "origin",
                "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
            }],
            "l4_services": [{"name": "relay", "upstream_pool": "origin", "udp": {}}]
        }))
        .expect("stage two test config");
        assert!(
            validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok()
        );

        config.management = Some(oxiroute_config::Management {
            bind: "127.0.0.1:9900".parse().expect("management address"),
            ui_dir: None,
        });
        assert!(
            validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok()
        );
        config.management = None;

        config.listeners.push(oxiroute_config::Listener {
            name: "http".into(),
            bind: oxiroute_config::ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().expect("address"),
            },
            protocol: Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        assert!(
            validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok()
        );

        config.stats = Some(Stats {
            binds: vec!["127.0.0.1:8404".parse().expect("statistics address")],
            admin_token_file: None,
            pages: Vec::new(),
        });
        assert!(
            validate_stage_one_config(&config.clone().validate().expect("valid config")).is_ok()
        );

        config.listeners[0].bind = oxiroute_config::ListenerBind::Udp {
            address: "127.0.0.1:8080".parse().expect("UDP address"),
        };
        config.listeners[0].protocol = Protocol::Udp;
        config.listeners[0].service = Some("relay".into());
        assert!(validate_stage_one_config(&config.validate().expect("valid config")).is_ok());
    }
}
