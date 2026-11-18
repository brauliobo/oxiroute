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

use log::{error, info, warn};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use oxiroute_config::ValidatedConfig;
use oxiroute_server::{
    ConfigEventFilter, ListenerReservations, RemoteRetired, RestartRequired, RuntimeMode,
    StartupOnly, StartupOnlyChange, WorkerNotice,
    config_coordinator::{
        AuthoredRevision, CanonicalConfigCoordinator, ConfigDiagnostic, ConfigDiagnosticSeverity,
        ConfigDiagnosticStage, ConfigLoadOutcome, ConfigRejection, EffectiveRevision,
        ResolvedConfigDocument,
    },
    startup_only_changes, startup_only_reasons,
};
use oxiroute_supervision::{
    CatalogError, GenerationId, GenerationLaunchDocument, GenerationRole, InstanceId,
    SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::{DescriptorManifest, InstanceToken, ListenerChanges};
use oxiroute_supervisor_master::{
    CONTROL_PROTOCOL_VERSION, MAX_AGGREGATED_EVENTS, Master, MasterConfig, MasterEvent,
    MasterState, WorkerInput, WorkerRole, WorkerStatus,
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

const PRODUCTION_LAUNCHER: &str = "/usr/lib/oxiroute/oxiroute-worker-launcher";
const TEST_LAUNCHER_ENV: &str = "OXIROUTE_INTERNAL_TEST_WORKER_LAUNCHER";

fn worker_launcher() -> PathBuf {
    std::env::var_os(TEST_LAUNCHER_ENV)
        .map_or_else(|| PathBuf::from(PRODUCTION_LAUNCHER), PathBuf::from)
}
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

/// Runs the master under the packaged launcher and the direct generation runtime without it.
///
/// With the launcher installed a configuration the master cannot run is an error, never a silent
/// downgrade to the direct runtime.
pub(crate) fn run_if_supported(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let coordinator = CanonicalConfigCoordinator::new(config_path)?;
    let document = load_document(&coordinator)?;
    if std::env::var_os("OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME").is_some()
        && !contains_rtmp_exec_profiles(&document.validated_config)
    {
        return crate::run(config_path);
    }
    let plan = config_plan(
        &document.validated_config,
        MasterRunner::launcher_available(),
        probe_cgroup_v2().status,
    )?;
    match plan {
        ConfigPlan::Supervised => MasterRunner::production()?.run_loaded(&coordinator, &document),
        ConfigPlan::Direct => {
            info!("supervised launcher is unavailable; using the direct generation runtime");
            crate::run(config_path)
        }
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
