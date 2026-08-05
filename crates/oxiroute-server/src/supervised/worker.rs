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

use oxiroute_config::{Config, ListenerBind, Protocol};
use oxiroute_server::{
    AdministrativeState, GenerationManager, ListenerRuntimeState, RuntimeGeneration,
    RuntimeSnapshot, worker_event_page,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision},
};
use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{InstanceToken, MAX_DESCRIPTOR_COUNT};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, MAX_STATUS_EVENTS,
    WorkerAdministrativeState, WorkerControl, WorkerEventRecord, WorkerGenerationStatus,
    WorkerLifecycle, WorkerListenerState, WorkerListenerStatus, WorkerMetrics, WorkerStatus,
};
use oxiroute_supervisor_process::WorkerIdentity;
use pingora::apps::AcceptGateClose;

use crate::{GenerationProcess, shutdown_generation_processes};

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
    if document.candidate_revision != metadata.revision {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_REVISION))?;
        return Err("canonical configuration revision did not match worker metadata".into());
    }
    if let Err(error) = validate_stage_one_config(&document.normalized_config) {
        control.acknowledge(&adoption, ControlOutcome::Rejected(REJECT_UNSUPPORTED))?;
        return Err(error.into());
    }
    let manager = GenerationManager::new();
    let candidate = match manager.prepare_adopted(*document, listeners) {
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
        manager.clone(),
        &stop,
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
    ) {
        drop(startup.take());
        let _ = shutdown_generation_processes(
            &manager,
            vec![process.take().expect("generation process is owned")],
            Instant::now() + SHUTDOWN_TIMEOUT,
        );
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
            drop(startup.take());
            let _ = shutdown_generation_processes(
                &manager,
                vec![process.take().expect("generation process is owned")],
                Instant::now() + SHUTDOWN_TIMEOUT,
            );
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
                drop(startup.take());
                let _ = shutdown_generation_processes(
                    &manager,
                    vec![process.take().expect("generation process is owned")],
                    Instant::now() + SHUTDOWN_TIMEOUT,
                );
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
                        let _ = shutdown_generation_processes(
                            &manager,
                            vec![process.take().expect("generation process is owned")],
                            Instant::now() + SHUTDOWN_TIMEOUT,
                        );
                        return Err(error.into());
                    }
                }
            }
            ControlPhase::Shutdown => {
                lifecycle = WorkerLifecycle::Stopping;
                drop(startup.take());
                let clean = shutdown_generation_processes(
                    &manager,
                    vec![process.take().expect("generation process is owned")],
                    Instant::now() + SHUTDOWN_TIMEOUT,
                );
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
                lifecycle = WorkerLifecycle::Reactivating;
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
    let metrics = runtime_snapshot.as_ref().ok().map(metrics_status);
    let mut listeners = runtime_snapshot
        .as_ref()
        .map_or_else(|_| Vec::new(), listener_statuses);
    append_configured_listener_statuses(&mut listeners, generation);

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
    let metrics_degraded = runtime_snapshot.is_err();
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

fn listener_statuses(snapshot: &RuntimeSnapshot) -> Vec<WorkerListenerStatus> {
    snapshot
        .listeners
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

fn append_configured_listener_statuses(
    statuses: &mut Vec<WorkerListenerStatus>,
    generation: &RuntimeGeneration,
) {
    let state = if generation.runtime_started() {
        WorkerListenerState::Listening
    } else {
        WorkerListenerState::Configured
    };
    let mut append = |name: String, protocol: String, bind: String| {
        if statuses.iter().any(|status| status.name == name) {
            return;
        }
        statuses.push(WorkerListenerStatus {
            name,
            protocol,
            bind,
            administrative_state: WorkerAdministrativeState::Ready,
            state,
            accepted_connections: 0,
            rejected_connections: 0,
            active_connections: 0,
            bytes_received: 0,
            bytes_sent: 0,
        });
    };
    let config = generation.config();
    if let Some(management) = &config.management {
        append(
            "@management".into(),
            "http".into(),
            management.bind.to_string(),
        );
    }
    if let Some(stats) = &config.stats {
        for (index, bind) in stats.binds.iter().enumerate() {
            append(format!("@stats-{index}"), "http".into(), bind.to_string());
        }
    }
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
    revision: ConfigRevision,
}

impl RuntimeMetadata {
    fn parse(arguments: &mut impl Iterator<Item = OsString>) -> Result<Self, Box<dyn Error>> {
        let config_path = arguments.next().ok_or("missing worker config path")?;
        if config_path.as_bytes().is_empty() || config_path.as_bytes().len() > MAX_CONFIG_PATH_BYTES
        {
            return Err("worker config path is empty or exceeds its bound".into());
        }
        let revision = parse_ascii(arguments.next(), "revision", 64)?.parse::<ConfigRevision>()?;
        if arguments.next().is_some() {
            return Err("trailing worker metadata".into());
        }
        Ok(Self {
            config_path: PathBuf::from(config_path),
            revision,
        })
    }
}

pub(super) fn validate_stage_one_config(config: &Config) -> Result<(), &'static str> {
    let descriptor_count = config
        .listeners
        .len()
        .saturating_add(usize::from(config.management.is_some()))
        .saturating_add(
            config
                .stats
                .as_ref()
                .map_or(0, |stats| stats.binds.len() + stats.pages.len()),
        );
    if descriptor_count > MAX_DESCRIPTOR_COUNT {
        return Err("Stage 2 worker listener descriptor limit is 64");
    }
    if config
        .listeners
        .iter()
        .any(|listener| matches!(&listener.bind, ListenerBind::Udp { .. }))
    {
        return Err("Stage 2 worker cannot adopt UDP listener descriptors");
    }
    if config
        .listeners
        .iter()
        .any(|listener| listener.protocol == Protocol::ForwardHttp3)
    {
        return Err("Stage 2 worker does not support HTTP/3 listener descriptors");
    }
    Ok(())
}

fn wait_for_generation_marker(
    generation: &RuntimeGeneration,
    process: &GenerationProcess,
) -> Result<(), Box<dyn Error>> {
    let expected_listener_count = config_listener_count(generation.config());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if generation.runtime_failed() || process.is_finished() {
            return Err(io::Error::other("generation listeners failed during startup").into());
        }
        let snapshot = generation.metrics().snapshot()?;
        if generation.runtime_started()
            && snapshot.listeners.len() == expected_listener_count
            && snapshot
                .listeners
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

fn config_listener_count(config: &Config) -> usize {
    config
        .listeners
        .len()
        .saturating_add(config.stats.as_ref().map_or(0, |stats| stats.pages.len()))
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
    use oxiroute_config::Stats;

    use super::*;

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
    fn stage_two_configuration_accepts_stream_listeners_and_statistics() {
        let mut config = Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        };
        assert!(validate_stage_one_config(&config).is_ok());

        config.management = Some(oxiroute_config::Management {
            bind: "127.0.0.1:9900".parse().expect("management address"),
            ui_dir: None,
        });
        assert_eq!(
            validate_stage_one_config(&config),
            Err("Stage 2 worker management API is not connected to the master")
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
        assert!(validate_stage_one_config(&config).is_ok());

        config.stats = Some(Stats {
            binds: Vec::new(),
            admin_token_file: None,
            pages: Vec::new(),
        });
        assert!(validate_stage_one_config(&config).is_ok());

        config.listeners[0].bind = oxiroute_config::ListenerBind::Udp {
            address: "127.0.0.1:8080".parse().expect("UDP address"),
        };
        assert_eq!(
            validate_stage_one_config(&config),
            Err("Stage 2 worker cannot adopt UDP listener descriptors")
        );
    }
}
