#![cfg(target_os = "linux")]

#[path = "../support/fixtures.rs"]
mod fixture_support;

use std::{
    fs,
    io::{self, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket as StdUdpSocket},
    os::unix::fs::PermissionsExt as _,
    os::{fd::AsRawFd as _, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bytes::{Buf as _, Bytes};
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ConfigDraft, DownstreamTimeoutPolicy,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpVersion, HttpVersionPolicy, L4Service, Listener, ListenerBind, Management, Protocol, Stats,
    StatsPage, StatsPageAdminPolicy, TlsProfile, TlsVersion, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamTls, ValidatedConfig,
};
use oxiroute_config_source::{ConfigFormat, render_config as render_source_config};
use oxiroute_server::{
    ListenerReservations,
    config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
};
use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::{DescriptorManifest, InstanceToken, ListenerChanges};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterEvent, MasterState, ShutdownProgress,
    WorkerInput, WorkerRole,
};
use oxiroute_supervisor_process::{WorkerCommand, WorkerIdentity, WorkerSpawner};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustix::fs::OFlags;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::time::{sleep, timeout};

mod configs;
mod h3_flows;
mod lifecycle;
mod reload;
mod replacement;
mod udp;

#[allow(clippy::wildcard_imports)]
use configs::*;
#[allow(clippy::wildcard_imports)]
use lifecycle::*;

const MARKER: &str = "--__oxiroute-worker-7f3c9d1e";
const TEST_RUNTIME_FAILURE_ENV: &str = "OXIROUTE_INTERNAL_TEST_RUNTIME_FAILURE";

fn render_config(format: ConfigFormat, config: &ValidatedConfig) -> String {
    render_source_config(format, config).expect("render validated config")
}
const TEST_LISTENER_DUPLICATION_FAILURE_ENV: &str =
    "OXIROUTE_INTERNAL_TEST_LISTENER_DUPLICATION_FAILURE";
const TEST_LAUNCHER_ENV: &str = "OXIROUTE_TEST_SUPERVISOR_LAUNCHER";
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN: [u8; 16] = [0x51; 16];

struct Harness {
    master: Master,
    worker_pid: u32,
    launcher_pid: u32,
    listener_targets: Vec<PathBuf>,
    _token_directory: Option<tempfile::TempDir>,
    /// Reservations behind the master's committed listener set, reused by the next reload.
    reservations: ListenerReservations,
    manifest: DescriptorManifest,
    /// Reservations of a replacement in flight, adopted when it commits.
    pending: Option<(ListenerReservations, DescriptorManifest)>,
    next_generation: u64,
}

impl Harness {
    fn launch(revision: String, inject_runtime_failure: bool) -> Self {
        Self::launch_at(config_path(), revision, inject_runtime_failure)
    }

    fn launch_at(config_path: &Path, revision: String, inject_runtime_failure: bool) -> Self {
        Self::launch_at_with_failures(config_path, revision, inject_runtime_failure, false)
    }

    fn launch_at_with_failures(
        config_path: &Path,
        revision: String,
        inject_runtime_failure: bool,
        inject_listener_duplication_failure: bool,
    ) -> Self {
        Self::launch_with_drain(
            config_path,
            revision,
            inject_runtime_failure,
            inject_listener_duplication_failure,
            Duration::from_secs(10),
        )
    }

    fn launch_with_drain(
        config_path: &Path,
        revision: String,
        inject_runtime_failure: bool,
        inject_listener_duplication_failure: bool,
        drain: Duration,
    ) -> Self {
        let coordinator =
            CanonicalConfigCoordinator::new(config_path).expect("fixture coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("canonical fixture was rejected");
        };
        let reservations = ListenerReservations::prepare(&document.validated_config, None)
            .expect("master listener reservations");
        let listener_targets = document
            .validated_config
            .as_draft()
            .listeners
            .iter()
            .map(|listener| {
                let descriptor = reservations
                    .get(&listener.name)
                    .expect("configured listener reservation")
                    .duplicate_owned_fd()
                    .expect("listener target descriptor");
                fs::read_link(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
                    .expect("master listener descriptor target")
            })
            .collect();
        let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
            .expect("production launcher implementation");
        let identity = WorkerIdentity {
            instance: InstanceToken(TOKEN),
            generation: GenerationId(1),
            protocol: CONTROL_PROTOCOL_VERSION,
        };
        let mut command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
            .expect("real oxiroute worker")
            .arg(MARKER)
            .arg(identity.generation.to_string())
            .arg(encode_token(TOKEN))
            .arg(config_path)
            .arg(revision);
        let token_directory = document
            .validated_config
            .as_draft()
            .management
            .as_ref()
            .map(|_| {
                let directory = tempfile::tempdir().expect("management token directory");
                let path = directory.path().join("management.token");
                fs::write(&path, "11".repeat(32)).expect("management token");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .expect("management token permissions");
                (directory, path)
            });
        if let Some((_, path)) = &token_directory {
            command = command.env("OXIROUTE_MANAGEMENT_TOKEN_FILE", path);
        }
        if inject_runtime_failure {
            command = command.env(TEST_RUNTIME_FAILURE_ENV, "1");
        }
        if inject_listener_duplication_failure {
            command = command.env(TEST_LISTENER_DUPLICATION_FAILURE_ENV, "1");
        }
        let listeners = reservations
            .clone()
            .into_stable_listeners(&document.validated_config)
            .expect("stable master listeners");
        let manifest = listeners.manifest().clone();
        let master = Master::launch(
            MasterConfig::new(
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(10),
                drain,
                Duration::from_secs(6),
            )
            .expect("master deadlines"),
            listeners,
            &mut factory,
            WorkerInput {
                instance_id: InstanceId::new("oxiroute-stage-2").expect("instance identity"),
                identity,
                command,
            },
            Instant::now(),
        )
        .expect("launch supervised worker");
        let worker_pid = master
            .worker_id(WorkerRole::Active)
            .expect("initial worker pid");
        let launcher_pid = master
            .worker_process_group_id(WorkerRole::Active)
            .expect("launcher process group");
        Self {
            master,
            worker_pid,
            launcher_pid,
            listener_targets,
            _token_directory: token_directory.map(|(directory, _)| directory),
            reservations,
            manifest,
            pending: None,
            next_generation: 2,
        }
    }

    /// Starts a replacement whose listeners are those of the configuration at `config_path`.
    ///
    /// Mirrors the supervised runner: the new set is prepared (binding only the sockets the
    /// running set does not own) before the candidate starts, and a bind failure is returned with
    /// nothing changed.
    fn start_reload(&mut self, config_path: &Path) -> io::Result<ListenerChanges> {
        let coordinator = CanonicalConfigCoordinator::new(config_path).expect("reload coordinator");
        let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
            panic!("reload configuration was rejected");
        };
        let config = &document.validated_config;
        let reservations = ListenerReservations::prepare(config, Some(&self.reservations))?;
        let listeners = reservations.clone().into_stable_listeners(config)?;
        let changes = self.manifest.changes(listeners.manifest());
        let manifest = listeners.manifest().clone();

        let generation = self.next_generation;
        self.next_generation += 1;
        let token = [u8::try_from(0x50 + generation).expect("test generation token"); 16];
        let identity = WorkerIdentity {
            instance: InstanceToken(token),
            generation: GenerationId(generation),
            protocol: CONTROL_PROTOCOL_VERSION,
        };
        let command = WorkerCommand::new(env!("CARGO_BIN_EXE_oxiroute"))
            .expect("real oxiroute worker")
            .arg(MARKER)
            .arg(generation.to_string())
            .arg(encode_token(token))
            .arg(config_path)
            .arg(document.effective_revision.to_string());
        let mut factory = WorkerSpawner::new(supervisor_launcher(), TEST_TIMEOUT)
            .expect("production launcher implementation");
        self.master
            .replace_with_listeners(
                &mut factory,
                WorkerInput {
                    instance_id: InstanceId::new(&format!(
                        "oxiroute-reload-candidate-{generation}"
                    ))
                    .expect("candidate identity"),
                    identity,
                    command,
                },
                listeners,
                Instant::now(),
            )
            .expect("start listener replacement");
        self.pending = Some((reservations, manifest));
        Ok(changes)
    }

    /// Polls until `done` accepts an event, adopting the pending listeners at commit.
    fn poll_events_until(
        &mut self,
        mut done: impl FnMut(&MasterEvent) -> bool,
    ) -> Vec<MasterEvent> {
        let started = Instant::now();
        let mut seen = Vec::new();
        loop {
            let events = self.master.poll(Instant::now()).expect("master poll");
            for event in &events {
                match event {
                    MasterEvent::ReplacementCommitted { .. } => {
                        if let Some((reservations, manifest)) = self.pending.take() {
                            self.reservations = reservations;
                            self.manifest = manifest;
                        }
                    }
                    MasterEvent::RollbackStarted { .. } | MasterEvent::SpawnFailed { .. } => {
                        self.pending = None;
                    }
                    _ => {}
                }
            }
            let finished = events.iter().any(&mut done);
            seen.extend(events);
            if finished {
                return seen;
            }
            assert!(
                started.elapsed() < Duration::from_mins(1),
                "master remained in {:?}; events: {seen:?}",
                self.master.state()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Waits for the commit. The master then drains the retired worker and returns to `Running`
    /// only once that worker has exited.
    fn wait_committed(&mut self) {
        self.poll_events_until(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }));
        assert_eq!(self.master.state(), MasterState::DrainingRetired);
    }

    fn wait_retired_worker_exit(&mut self) {
        self.poll_events_until(|event| matches!(event, MasterEvent::ReplacementCompleted { .. }));
        self.poll_until(MasterState::Running);
    }

    fn poll_until(&mut self, expected: MasterState) {
        let started = Instant::now();
        while self.master.state() != expected {
            self.master.poll(Instant::now()).expect("master poll");
            assert!(
                started.elapsed() < TEST_TIMEOUT,
                "master remained in {:?}",
                self.master.state()
            );
            thread::sleep(POLL_INTERVAL);
        }
        self.master.poll(Instant::now()).expect("final master poll");
    }

    fn verify_reaped(&self) {
        wait_process_gone(self.worker_pid);
        wait_process_gone(self.launcher_pid);
    }
}

/// Retries `attempt` until it succeeds. Resources released by a killed or dropped owner stay
/// held for a moment by processes a concurrent test has forked and not yet exec'd.
fn eventually<T, E: std::fmt::Display>(what: &str, mut attempt: impl FnMut() -> Result<T, E>) -> T {
    let started = Instant::now();
    loop {
        match attempt() {
            Ok(value) => return value,
            Err(error) => {
                assert!(
                    started.elapsed() < TEST_TIMEOUT,
                    "{what} did not happen: {error}"
                );
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

fn assert_udp_can_bind_again(address: SocketAddr) {
    eventually(&format!("{address} release"), || {
        StdUdpSocket::bind(address)
    });
}

/// One listener set for the reload tests: HTTP listeners that all answer `body` (or proxy to
/// `origin` when set), plus an optional TCP relay `(listener name, listener address, upstream
/// address)`.
struct ReloadShape<'a> {
    http: &'a [(&'a str, SocketAddr)],
    relay: Option<(&'a str, SocketAddr, SocketAddr)>,
    origin: Option<SocketAddr>,
    body: &'a str,
}
