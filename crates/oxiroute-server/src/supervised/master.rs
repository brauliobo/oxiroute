use std::{
    collections::HashSet,
    error::Error,
    ffi::OsString,
    fs::File,
    io::{self, Read as _},
    os::fd::{BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use log::{info, warn};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use oxiroute_config::ValidatedConfig;
use oxiroute_server::{
    ListenerReservations, RemoteRetired, RuntimeMode, WorkerNotice,
    config_coordinator::{
        AuthoredRevision, CanonicalConfigCoordinator, ConfigDiagnostic, ConfigDiagnosticSeverity,
        ConfigDiagnosticStage, ConfigLoadOutcome, ConfigRejection, EffectiveRevision,
        ResolvedConfigDocument,
    },
};
use oxiroute_supervision::{
    CatalogError, GenerationId, GenerationLaunchDocument, GenerationRole, InstanceId,
    SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::{DescriptorManifest, InstanceToken, ListenerChanges};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, Master, MasterConfig, MasterEvent, MasterState, WorkerInput,
    WorkerRole, WorkerStatus,
};
use oxiroute_supervisor_process::{
    CgroupV2ProbeStatus, WorkerCommand, WorkerIdentity, WorkerSpawner, probe_cgroup_v2,
};
use rustix::event::{EventfdFlags, eventfd};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};
use thiserror::Error;

use super::worker;

// The package installs this fixed path. Unsupported descriptor topologies continue through the
// existing generation runtime, and development installations without the package use the same
// fallback.
const PRODUCTION_LAUNCHER: &str = "/usr/lib/oxiroute/oxiroute-worker-launcher";
const MASTER_INSTANCE_ID: &str = "oxiroute-stage-3";
const INITIAL_GENERATION: GenerationId = GenerationId(1);
const WORKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const CONFIG_RELOAD_MAX_DEBOUNCE: Duration = Duration::from_secs(2);
const CONFIG_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const RELOAD_DRAIN_ENV: &str = "OXIROUTE_RELOAD_DRAIN_SECONDS";
const DEFAULT_RELOAD_DRAIN: Duration = Duration::from_hours(3);
const MAX_RELOAD_DRAIN: Duration = Duration::from_hours(24);
/// The master kills a retired worker this long after the deadline at which the worker closes itself.
pub(super) const RETIREMENT_BACKSTOP: Duration = Duration::from_secs(10);

/// Runs the production master for one eligible canonical configuration.
pub(crate) fn run_master(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    supervised_eligibility(&document.validated_config, probe_cgroup_v2().status)?;
    MasterRunner::production()?.run_loaded(&coordinator, &document)
}

/// Runs the master when the configuration is eligible and otherwise preserves the current server.
///
/// The fixed packaged launcher is the activation gate. An eligible configuration without that
/// launcher preserves the direct generation runtime for development installations.
pub(crate) fn run_if_supported(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    let launcher_available = MasterRunner::launcher_available();
    let plan = config_plan(
        &document.validated_config,
        launcher_available,
        probe_cgroup_v2().status,
    )?;
    if std::env::var_os("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME").is_some()
        && !contains_rtmp_exec_profiles(&document.validated_config)
    {
        return crate::run(config_path);
    }
    match plan {
        ConfigPlan::Supervised => MasterRunner::production()?.run_loaded(&coordinator, &document),
        ConfigPlan::Direct if !launcher_available => {
            info!("supervised launcher is unavailable; using the direct generation runtime");
            crate::run(config_path)
        }
        ConfigPlan::Direct => crate::run(config_path),
    }
}

mod catalog;
mod monitors;
mod plan;
mod retired;
mod runner;
mod setup;
mod wake;

#[allow(clippy::wildcard_imports)]
use catalog::*;
#[allow(clippy::wildcard_imports)]
use monitors::*;
#[allow(clippy::wildcard_imports)]
use plan::*;
#[allow(clippy::wildcard_imports)]
use retired::*;
#[allow(clippy::wildcard_imports)]
use runner::*;
pub(super) use setup::retirement_window;
#[allow(clippy::wildcard_imports)]
use setup::*;
use wake::Waker;

#[cfg(test)]
mod tests;
