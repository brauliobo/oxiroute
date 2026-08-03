use std::{
    error::Error,
    fs::File,
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use log::warn;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
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
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterEvent, MasterState, WorkerInput,
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
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const CONFIG_RELOAD_MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const CONFIG_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(30);

/// Runs the production master for one eligible canonical configuration.
pub(crate) fn run_master(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    eligibility(&document.normalized_config)?;
    MasterRunner::production()?.run_loaded(&coordinator, &document)
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
        Ok(()) => MasterRunner::production()?.run_loaded(&coordinator, &document),
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
        coordinator: &CanonicalConfigCoordinator,
        document: &CanonicalConfigDocument,
    ) -> Result<(), Box<dyn Error>> {
        let config = &document.normalized_config;
        let reservations = ListenerReservations::prepare(config, None)?;
        let listeners = reservations.into_stable_listeners(config)?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = worker_identity(token)?;
        let command = self.build_worker_command(
            coordinator.canonical_path(),
            &document.candidate_revision,
            identity,
        )?;
        let master_config = master_config()?;
        let mut factory = WorkerSpawner::new(&self.launcher_path, WORKER_HANDSHAKE_TIMEOUT)?;
        let mut reload = ConfigReloadMonitor::start(coordinator.canonical_path())?;
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
            Ok(mut master) => self.drive(
                &mut master,
                &signals.stop,
                coordinator,
                document,
                &mut factory,
                &mut reload,
            ),
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

    fn drive(
        &self,
        master: &mut Master,
        stop: &AtomicBool,
        coordinator: &CanonicalConfigCoordinator,
        initial_document: &CanonicalConfigDocument,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
    ) -> Result<(), Box<dyn Error>> {
        let result = self.drive_inner(master, stop, coordinator, initial_document, factory, reload);
        if result.is_err() && !matches!(master.state(), MasterState::Stopped | MasterState::Failed)
        {
            cleanup_master(master);
        }
        result
    }

    fn drive_inner(
        &self,
        master: &mut Master,
        stop: &AtomicBool,
        coordinator: &CanonicalConfigCoordinator,
        initial_document: &CanonicalConfigDocument,
        factory: &mut WorkerSpawner,
        reload: &mut ConfigReloadMonitor,
    ) -> Result<(), Box<dyn Error>> {
        let mut reload_state = ReloadState::new(initial_document);
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
                    let now = Instant::now();
                    let events = master.poll(now)?;
                    reload_state.apply_events(&events);
                    if reload_state.pending.is_none() && reload.next_trigger(now) {
                        self.reconcile(master, coordinator, &mut reload_state, factory)?;
                    }
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
                MasterState::Failed => return Err(master_failure("runtime").into()),
                MasterState::Stopped => {
                    return Err(master_failure("unexpected early shutdown").into());
                }
                _ => {
                    let events = master.poll(Instant::now())?;
                    reload_state.apply_events(&events);
                    thread::sleep(MASTER_POLL_INTERVAL);
                }
            }
        }
    }

    fn reconcile(
        &self,
        master: &mut Master,
        coordinator: &CanonicalConfigCoordinator,
        reload_state: &mut ReloadState,
        factory: &mut WorkerSpawner,
    ) -> Result<(), Box<dyn Error>> {
        let document = match coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(_) => {
                warn!("supervised master ignored a rejected configuration reload");
                return Ok(());
            }
        };
        let revision = document.candidate_revision.clone();
        if revision == reload_state.active_revision {
            return Ok(());
        }
        if let Err(error) = eligibility(&document.normalized_config) {
            warn!("supervised master ignored an unsupported configuration reload: {error}");
            return Ok(());
        }
        if !same_listener_manifest(&reload_state.active_config, &document.normalized_config) {
            warn!("supervised master ignored a configuration reload that changes listeners");
            return Ok(());
        }

        let generation = GenerationId(reload_state.next_generation);
        reload_state.next_generation = reload_state
            .next_generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("supervised worker generation exhausted"))?;
        let token = generate_instance_token()?;
        let (instance_id, identity) = replacement_worker_identity(token, generation)?;
        let command =
            self.build_worker_command(coordinator.canonical_path(), &revision, identity)?;
        let candidate = WorkerInput {
            instance_id,
            identity,
            command,
        };
        if let Err(error) = master.replace(factory, candidate, Instant::now()) {
            if master.state() != MasterState::Running {
                return Err(error.into());
            }
            warn!("supervised master could not start a configuration replacement: {error}");
            return Ok(());
        }
        reload_state.pending = Some(PendingReplacement {
            revision,
            config: document.normalized_config.clone(),
        });
        Ok(())
    }
}

struct PendingReplacement {
    revision: ConfigRevision,
    config: Config,
}

struct ReloadState {
    active_config: Config,
    active_revision: ConfigRevision,
    pending: Option<PendingReplacement>,
    next_generation: u64,
}

impl ReloadState {
    fn new(document: &CanonicalConfigDocument) -> Self {
        Self {
            active_config: document.normalized_config.clone(),
            active_revision: document.candidate_revision.clone(),
            pending: None,
            next_generation: INITIAL_GENERATION.0 + 1,
        }
    }

    fn apply_events(&mut self, events: &[MasterEvent]) {
        for event in events {
            match event {
                MasterEvent::ReplacementCommitted { .. } => {
                    if let Some(replacement) = self.pending.take() {
                        self.active_config = replacement.config;
                        self.active_revision = replacement.revision;
                    }
                }
                MasterEvent::RollbackCompleted { .. } => {
                    self.pending.take();
                }
                _ => {}
            }
        }
    }
}

struct ConfigReloadMonitor {
    _watcher: RecommendedWatcher,
    events: Receiver<()>,
    next_reconciliation: Instant,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
}

impl ConfigReloadMonitor {
    fn start(path: &Path) -> notify::Result<Self> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let target = path.file_name().map(std::borrow::ToOwned::to_owned);
        let (wake, events) = mpsc::sync_channel(1);
        let watcher_wake = wake.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let relevant = match event {
                    Err(_) => true,
                    Ok(event) => {
                        event.need_rescan()
                            || (!event.kind.is_access()
                                && (event.paths.is_empty()
                                    || event
                                        .paths
                                        .iter()
                                        .any(|path| path.file_name() == target.as_deref())))
                    }
                };
                if relevant {
                    let _ = watcher_wake.try_send(());
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
            next_reconciliation: Instant::now(),
            first_event: None,
            last_event: None,
        })
    }

    fn next_trigger(&mut self, now: Instant) -> bool {
        if self.events.try_iter().next().is_some() {
            self.first_event.get_or_insert(now);
            self.last_event = Some(now);
        }
        if let (Some(first_event), Some(last_event)) = (self.first_event, self.last_event) {
            let quiet = now.saturating_duration_since(last_event) >= CONFIG_RELOAD_DEBOUNCE;
            let bounded = now.saturating_duration_since(first_event) >= CONFIG_RELOAD_MAX_DEBOUNCE;
            if quiet || bounded {
                self.first_event = None;
                self.last_event = None;
                self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
                return true;
            }
            return false;
        }
        let periodic = now >= self.next_reconciliation;
        if periodic {
            self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
        }
        periodic
    }
}

fn same_listener_manifest(active: &Config, candidate: &Config) -> bool {
    active.listeners.len() == candidate.listeners.len()
        && active
            .listeners
            .iter()
            .zip(&candidate.listeners)
            .all(|(active, candidate)| {
                active.name == candidate.name && active.bind == candidate.bind
            })
}

fn master_config() -> Result<MasterConfig, Box<dyn Error>> {
    Ok(MasterConfig::new(
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::from_secs(10),
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

fn replacement_worker_identity(
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
    fn replacement_identity_is_unique_and_monotonic() {
        let (_, first) = replacement_worker_identity(InstanceToken([0; 16]), GenerationId(2))
            .expect("first identity");
        let (second_id, second) =
            replacement_worker_identity(InstanceToken([1; 16]), GenerationId(3))
                .expect("second identity");

        assert_eq!(first.generation, GenerationId(2));
        assert_eq!(second.generation, GenerationId(3));
        assert_eq!(second_id.as_str(), "oxiroute-stage-3-3");
        assert_ne!(first.instance, second.instance);
    }

    #[test]
    fn replacement_requires_the_same_listener_manifest() {
        let mut active = config();
        active
            .listeners
            .push(listener("http", Protocol::Http, None));
        let mut candidate = active.clone();
        candidate.listeners[0].service = Some("changed".into());
        assert!(same_listener_manifest(&active, &candidate));

        candidate.listeners[0].name = "renamed".into();
        assert!(!same_listener_manifest(&active, &candidate));

        candidate = active.clone();
        candidate.listeners[0].bind = ListenerBind::Socket {
            address: SocketAddr::from(([127, 0, 0, 1], 8081)),
        };
        assert!(!same_listener_manifest(&active, &candidate));
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
