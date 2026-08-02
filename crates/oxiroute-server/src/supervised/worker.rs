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

use oxiroute_config::{Config, Protocol};
use oxiroute_server::{
    GenerationManager, RuntimeGeneration,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigRevision},
};
use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{InstanceToken, MAX_DESCRIPTOR_COUNT};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, ControlOutcome, ControlPhase, WorkerControl,
};
use oxiroute_supervisor_process::WorkerIdentity;

use crate::{GenerationProcess, shutdown_generation_processes};

const MAX_CONFIG_PATH_BYTES: usize = 4 * 1024;
const REJECT_INVALID_STATE: u8 = 1;
const REJECT_CONFIG: u8 = 2;
const REJECT_REVISION: u8 = 3;
const REJECT_RUNTIME: u8 = 4;
const REJECT_UNSUPPORTED: u8 = 5;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
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
    control.acknowledge(&adoption, ControlOutcome::Accepted)?;

    let inject_runtime_failure = std::env::var_os(TEST_RUNTIME_FAILURE_ENV).is_some();
    loop {
        if generation.runtime_failed()
            || process.as_ref().is_some_and(GenerationProcess::is_finished)
        {
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
                        control.acknowledge(&request, ControlOutcome::Accepted)?;
                        if inject_runtime_failure {
                            generation.mark_runtime_failed();
                        }
                    }
                    Err(error) => {
                        control.acknowledge(&request, ControlOutcome::Rejected(REJECT_RUNTIME))?;
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
                return if clean {
                    Ok(())
                } else {
                    Err("generation shutdown did not complete cleanly".into())
                };
            }
            ControlPhase::Quiesce | ControlPhase::Drain | ControlPhase::Reactivate => {
                control.acknowledge(&request, phase_outcome(request.phase()))?;
            }
            ControlPhase::AdoptListeners | ControlPhase::Activate => {
                control.acknowledge(&request, ControlOutcome::Rejected(REJECT_INVALID_STATE))?;
            }
        }
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
    if config.listeners.len() > MAX_DESCRIPTOR_COUNT {
        return Err("Stage 2 worker listener descriptor limit is 64");
    }
    if config.management.is_some()
        || config.stats.is_some()
        || !config.certificates.is_empty()
        || !config.tls_profiles.is_empty()
        || !config.cache_stores.is_empty()
        || !config.upstream_pools.is_empty()
        || !config.forward_proxy_services.is_empty()
        || !config.rtmp_services.is_empty()
        || !config.l4_services.is_empty()
    {
        return Err("Stage 2 worker configuration contains an unsupported service or subsystem");
    }
    if config
        .listeners
        .iter()
        .any(|listener| listener.protocol != Protocol::Http || listener.tls_profile.is_some())
    {
        return Err("Stage 2 worker supports only plaintext HTTP traffic listeners");
    }
    Ok(())
}

fn wait_for_generation_marker(
    generation: &RuntimeGeneration,
    process: &GenerationProcess,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if generation.runtime_failed() || process.is_finished() {
            return Err(io::Error::other("generation listeners failed during startup").into());
        }
        let snapshot = generation.metrics().snapshot()?;
        if generation.runtime_started()
            && snapshot.listeners.len() == generation.config().listeners.len()
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

fn phase_outcome(phase: ControlPhase) -> ControlOutcome {
    match phase {
        ControlPhase::Quiesce | ControlPhase::Drain | ControlPhase::Reactivate => {
            ControlOutcome::Rejected(REJECT_UNSUPPORTED)
        }
        _ => ControlOutcome::Accepted,
    }
}

#[cfg(test)]
mod tests {
    use oxiroute_config::Stats;

    use super::*;

    #[test]
    fn deferred_lifecycle_phases_are_explicitly_unsupported() {
        for phase in [
            ControlPhase::Quiesce,
            ControlPhase::Drain,
            ControlPhase::Reactivate,
        ] {
            assert!(matches!(
                phase_outcome(phase),
                ControlOutcome::Rejected(REJECT_UNSUPPORTED)
            ));
        }
    }

    #[test]
    fn stage_two_configuration_accepts_marker_and_plain_http_listeners() {
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

        config.listeners.push(oxiroute_config::Listener {
            name: "http".into(),
            bind: oxiroute_config::ListenerBind::Socket {
                address: "127.0.0.1:8080".parse().expect("address"),
            },
            protocol: Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        assert!(validate_stage_one_config(&config).is_ok());

        config.stats = Some(Stats {
            binds: Vec::new(),
            admin_token_file: None,
            pages: Vec::new(),
        });
        assert!(validate_stage_one_config(&config).is_err());
    }
}
