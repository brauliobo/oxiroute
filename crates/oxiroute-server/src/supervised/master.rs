use std::{
    error::Error,
    fs::File,
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use oxiroute_config::Config;
use oxiroute_server::{
    ListenerReservations,
    config_coordinator::{
        CanonicalConfigCoordinator, CanonicalConfigDocument, ConfigLoadOutcome, ConfigRevision,
    },
};
use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::InstanceToken;
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterState, WorkerInput,
};
use oxiroute_supervisor_process::{WorkerCommand, WorkerIdentity, WorkerSpawner};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};
use thiserror::Error;

use super::worker;

// This adapter is intentionally not selected by `main` until packaging installs the fixed
// launcher path. Unsupported configurations continue through the existing generation runtime.
const PRODUCTION_LAUNCHER: &str = "/usr/lib/oxiroute/oxiroute-worker-launcher";
const MASTER_INSTANCE_ID: &str = "oxiroute-stage-3";
const INITIAL_GENERATION: GenerationId = GenerationId(1);
const WORKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MASTER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs the production master for one eligible canonical configuration.
pub(crate) fn run_master(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    eligibility(&document.normalized_config)?;
    MasterRunner::production()?.run_loaded(coordinator.canonical_path(), &document)
}

/// Runs the master when the configuration is eligible and otherwise preserves the current server.
///
/// This helper is not wired into the public `serve` dispatch yet. When it is selected by the
/// packaging gate, an eligible configuration must return launcher errors rather than silently
/// falling back to the generation runtime.
pub(crate) fn run_if_supported(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    match eligibility(&document.normalized_config) {
        Ok(()) => MasterRunner::production()?.run_loaded(coordinator.canonical_path(), &document),
        Err(_unsupported) => crate::run(config_path),
    }
}

#[derive(Debug, Eq, Error, PartialEq)]
#[error("supervised master does not support this configuration: {reason}")]
struct UnsupportedConfig {
    reason: &'static str,
}

fn eligibility(config: &Config) -> Result<(), UnsupportedConfig> {
    worker::validate_stage_one_config(config).map_err(|reason| UnsupportedConfig { reason })
}

fn load_document(
    coordinator: &CanonicalConfigCoordinator,
) -> Result<Box<CanonicalConfigDocument>, Box<dyn Error>> {
    match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => Ok(document),
        ConfigLoadOutcome::Rejected(rejection) => {
            let message = rejection.diagnostics.first().map_or_else(
                || "canonical configuration was rejected".to_owned(),
                |diagnostic| {
                    format!(
                        "canonical configuration was rejected ({}): {}",
                        diagnostic.code, diagnostic.message
                    )
                },
            );
            Err(io::Error::new(io::ErrorKind::InvalidData, message).into())
        }
    }
}

struct MasterRunner {
    launcher_path: PathBuf,
    executable_path: PathBuf,
}

impl MasterRunner {
    fn production() -> Result<Self, Box<dyn Error>> {
        let executable_path = std::env::current_exe()?;
        if !executable_path.is_absolute() {
            return Err(
                io::Error::other("current oxiroute executable path is not absolute").into(),
            );
        }
        Ok(Self {
            launcher_path: PathBuf::from(PRODUCTION_LAUNCHER),
            executable_path,
        })
    }

    #[cfg(test)]
    fn new_for_test(
        launcher_path: impl Into<PathBuf>,
        executable_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            launcher_path: launcher_path.into(),
            executable_path: executable_path.into(),
        }
    }

    fn run_loaded(
        &self,
        config_path: &Path,
        document: &CanonicalConfigDocument,
    ) -> Result<(), Box<dyn Error>> {
        let config = &document.normalized_config;
        let reservations = ListenerReservations::prepare(config, None)?;
        let listeners = reservations.into_stable_listeners(config)?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = worker_identity(token)?;
        let command =
            self.build_worker_command(config_path, &document.candidate_revision, identity)?;
        let master_config = master_config()?;
        let mut factory = WorkerSpawner::new(&self.launcher_path, WORKER_HANDSHAKE_TIMEOUT)?;
        let signals = SignalMonitor::new()?;
        let launch = Master::launch(
            master_config,
            listeners,
            &mut factory,
            WorkerInput {
                instance_id,
                identity,
                command,
            },
            Instant::now(),
        );
        let result = match launch {
            Ok(mut master) => Self::drive(&mut master, &signals.stop),
            Err(error) => Err(error.into()),
        };
        signals.finish(result)
    }

    fn build_worker_command(
        &self,
        config_path: &Path,
        revision: &ConfigRevision,
        identity: WorkerIdentity,
    ) -> Result<WorkerCommand, Box<dyn Error>> {
        Ok(WorkerCommand::new(&self.executable_path)?
            .arg(super::MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(identity.instance))
            .arg(config_path)
            .arg(revision.to_string()))
    }

    fn drive(master: &mut Master, stop: &AtomicBool) -> Result<(), Box<dyn Error>> {
        let result = Self::drive_inner(master, stop);
        if result.is_err() && !matches!(master.state(), MasterState::Stopped | MasterState::Failed)
        {
            cleanup_master(master);
        }
        result
    }

    fn drive_inner(master: &mut Master, stop: &AtomicBool) -> Result<(), Box<dyn Error>> {
        while master.state() != MasterState::Running {
            if master.state() == MasterState::Failed {
                return Err(master_failure("startup").into());
            }
            if stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            master.poll(Instant::now())?;
            thread::sleep(MASTER_POLL_INTERVAL);
        }

        loop {
            if stop.load(Ordering::Acquire) {
                return shutdown_master(master);
            }
            match master.state() {
                MasterState::Running => {
                    master.poll(Instant::now())?;
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
                MasterState::Failed => return Err(master_failure("runtime").into()),
                MasterState::Stopped => {
                    return Err(master_failure("unexpected early shutdown").into());
                }
                _ => {
                    master.poll(Instant::now())?;
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
            }
        }
    }
}

fn master_config() -> Result<MasterConfig, Box<dyn Error>> {
    Ok(MasterConfig::new(
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(10),
        Duration::from_secs(1),
        Duration::from_secs(6),
    )?)
}

fn worker_identity(token: InstanceToken) -> Result<(InstanceId, WorkerIdentity), Box<dyn Error>> {
    Ok((
        InstanceId::new(MASTER_INSTANCE_ID)?,
        WorkerIdentity {
            instance: token,
            generation: INITIAL_GENERATION,
            protocol: CONTROL_PROTOCOL_VERSION,
        },
    ))
}

fn generate_instance_token() -> io::Result<InstanceToken> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(InstanceToken(bytes))
}

fn encode_token(token: InstanceToken) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(32);
    for byte in token.0 {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn shutdown_master(master: &mut Master) -> Result<(), Box<dyn Error>> {
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

fn cleanup_master(master: &mut Master) {
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

fn master_failure(phase: &str) -> io::Error {
    io::Error::other(format!("supervised master failed during {phase}"))
}

struct SignalMonitor {
    stop: Arc<AtomicBool>,
    handle: Handle,
    thread: JoinHandle<()>,
}

impl SignalMonitor {
    fn new() -> Result<Self, Box<dyn Error>> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut signals = Signals::new([SIGTERM, SIGINT])?;
        let handle = signals.handle();
        let signal_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("oxiroute-master-signals".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    signal_stop.store(true, Ordering::Release);
                }
            })?;
        Ok(Self {
            stop,
            handle,
            thread,
        })
    }

    fn finish(self, result: Result<(), Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
        let Self {
            stop: _,
            handle,
            thread,
        } = self;
        handle.close();
        let signal_result = thread.join();
        match (result, signal_result) {
            (Err(error), _) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(_)) => {
                Err(io::Error::other("master signal thread terminated unexpectedly").into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf, str::FromStr as _};

    use oxiroute_config::{Config, DownstreamTimeoutPolicy, Listener, ListenerBind, Protocol};

    use super::*;

    fn config() -> Config {
        Config {
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
        }
    }

    fn listener(name: &str, protocol: Protocol, tls_profile: Option<&str>) -> Listener {
        Listener {
            name: name.into(),
            bind: ListenerBind::Socket {
                address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            protocol,
            service: None,
            tls_profile: tls_profile.map(str::to_owned),
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }
    }

    #[test]
    fn eligibility_accepts_only_plain_http_listener_configuration() {
        let mut config = config();
        config
            .listeners
            .push(listener("http", Protocol::Http, None));
        assert_eq!(eligibility(&config), Ok(()));

        config.listeners[0].protocol = Protocol::Rtmp;
        assert_eq!(
            eligibility(&config),
            Err(UnsupportedConfig {
                reason: "Stage 2 worker supports only plaintext HTTP traffic listeners",
            })
        );
    }

    #[test]
    fn eligibility_uses_the_worker_descriptor_limit() {
        let mut config = config();
        config.listeners = (0..=oxiroute_supervision_unix::MAX_DESCRIPTOR_COUNT)
            .map(|index| listener(&format!("listener-{index}"), Protocol::Http, None))
            .collect();
        assert!(matches!(
            eligibility(&config),
            Err(UnsupportedConfig {
                reason: "Stage 2 worker listener descriptor limit is 64",
            })
        ));
    }

    #[test]
    fn worker_identity_preserves_token_and_fixed_generation() {
        let token = InstanceToken([0xab; 16]);
        let (instance_id, identity) = worker_identity(token).expect("worker identity");
        assert_eq!(instance_id.as_str(), MASTER_INSTANCE_ID);
        assert_eq!(identity.instance, token);
        assert_eq!(identity.generation, INITIAL_GENERATION);
        assert_eq!(identity.protocol, CONTROL_PROTOCOL_VERSION);
    }

    #[test]
    fn production_runner_uses_the_fixed_launcher_and_absolute_executable() {
        let runner = MasterRunner::production().expect("production runner");
        assert_eq!(runner.launcher_path, Path::new(PRODUCTION_LAUNCHER));
        assert!(runner.executable_path.is_absolute());
    }

    #[test]
    fn worker_command_contains_metadata_but_not_launcher_configuration() {
        let executable = std::env::current_exe().expect("test executable");
        let launcher = PathBuf::from("/test-only/oxiroute-worker-launcher");
        let runner = MasterRunner::new_for_test(launcher.clone(), executable.clone());
        let revision = ConfigRevision::from_str(&"a".repeat(64)).expect("revision");
        let (_, identity) = worker_identity(InstanceToken([0xcd; 16])).expect("identity");
        let command = runner
            .build_worker_command(Path::new("/etc/oxiroute/oxiroute.kdl"), &revision, identity)
            .expect("worker command");
        let debug = format!("{command:?}");

        assert!(debug.contains(executable.to_string_lossy().as_ref()));
        assert!(debug.contains(super::super::MARKER));
        assert!(debug.contains(", \"1\","));
        assert!(debug.contains(&"cd".repeat(16)));
        assert!(debug.contains("/etc/oxiroute/oxiroute.kdl"));
        assert!(debug.contains(&"a".repeat(64)));
        assert!(debug.contains("env: []"));
        assert!(!debug.contains(launcher.to_string_lossy().as_ref()));
    }
}
