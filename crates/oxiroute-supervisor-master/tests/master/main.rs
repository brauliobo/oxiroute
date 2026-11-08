use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::{self, Read as _},
    net::{SocketAddr, TcpListener, TcpStream},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use oxiroute_supervision::{
    GenerationId, GenerationLaunchDocument, GenerationRole, InstanceId, Revision,
    SupervisedGenerationCatalog,
};
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot,
    InstanceToken, SlotId,
};
use oxiroute_supervisor_master::{
    ActionError, ActionExecutor, ActionKind, CONTROL_PROTOCOL_VERSION, ControlPhase, FailurePhase,
    Master, MasterConfig, MasterError, MasterEvent, MasterState, PreparationError, PreparationStep,
    ShutdownProgress, StableListeners, SystemActionExecutor, WorkerInput, WorkerRole, WorkerState,
};
use oxiroute_supervisor_process::{
    DEFAULT_CGROUP_V2_ROOT, WorkerCommand, WorkerIdentity, WorkerProcess, WorkerSpawner,
    probe_cgroup_v2,
};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

mod faults;
mod lifecycle;
mod replacement;

#[allow(clippy::wildcard_imports)]
use faults::*;

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness<E: ActionExecutor = SystemActionExecutor> {
    _temporary: tempfile::TempDir,
    listener_targets: Vec<PathBuf>,
    tcp_address: SocketAddr,
    unix_path: PathBuf,
    kept: KeptListeners,
    factory: WorkerSpawner,
    master: Master<E>,
}

fn is_zombie(pid: u32) -> bool {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat.rsplit_once(") ")
        .is_some_and(|(_, rest)| rest.starts_with('Z'))
}

fn wait_for_launcher_exit(launcher: u32) {
    let started = Instant::now();
    while !is_zombie(launcher) {
        assert!(
            started.elapsed() < TEST_TIMEOUT,
            "launcher {launcher} did not exit"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

/// Test-side handles on the sockets the master owns, so a later listener set can keep them.
struct KeptListeners {
    tcp: TcpListener,
    unix: UnixListener,
    path_guard: Arc<UnixPathGuard>,
}

impl Harness<SystemActionExecutor> {
    fn launch(active_behavior: &str) -> Self {
        Self::launch_with_executor(active_behavior, SystemActionExecutor)
    }

    #[cfg(target_os = "linux")]
    fn launch_contained(active_behavior: &str, pid_file: &Path) -> Self {
        Self::launch_input_with_executor(
            contained_worker("a", 1, active_behavior, pid_file),
            SystemActionExecutor,
        )
    }
}

impl<E: ActionExecutor> Harness<E> {
    fn launch_with_executor(active_behavior: &str, executor: E) -> Self {
        Self::launch_input_with_executor(worker("a", 1, active_behavior), executor)
    }

    fn launch_input_with_executor(active: WorkerInput<WorkerCommand>, executor: E) -> Self {
        let (temporary, listeners, listener_targets, tcp_address, unix_path, kept) = listeners();
        let config = MasterConfig::new(
            Duration::from_millis(300),
            Duration::from_millis(300),
            Duration::from_millis(300),
            Duration::from_millis(300),
            Duration::from_millis(300),
        )
        .unwrap();
        let mut factory = WorkerSpawner::new(
            env!("CARGO_BIN_EXE_master-launcher-fixture"),
            Duration::from_secs(2),
        )
        .unwrap();
        let master = Master::launch_with_executor(
            config,
            listeners,
            executor,
            &mut factory,
            active,
            Instant::now(),
        )
        .unwrap();
        let mut harness = Self {
            _temporary: temporary,
            listener_targets,
            tcp_address,
            unix_path,
            kept,
            factory,
            master,
        };
        let events = harness.until(|master| master.state() == MasterState::Running);
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::InitialActivated { instance_id } if instance_id.as_str() == "a"
        )));
        assert_served_by(&harness, 1);
        harness
    }

    fn replace(&mut self, behavior: &str, generation: u64) {
        self.master
            .replace(
                &mut self.factory,
                worker("b", generation, behavior),
                Instant::now(),
            )
            .unwrap();
    }

    fn replace_with_listeners(&mut self, behavior: &str, generation: u64, next: NextSet) {
        let listeners = self.kept.next_set(self.tcp_address, &self.unix_path, next);
        self.master
            .replace_with_listeners(
                &mut self.factory,
                worker("b", generation, behavior),
                listeners,
                Instant::now(),
            )
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    fn replace_contained(&mut self, behavior: &str, generation: u64, pid_file: &Path) {
        self.master
            .replace(
                &mut self.factory,
                contained_worker("b", generation, behavior, pid_file),
                Instant::now(),
            )
            .unwrap();
    }

    fn until(&mut self, predicate: impl FnMut(&Master<E>) -> bool) -> Vec<MasterEvent> {
        let mut events = self.poll_until(predicate);
        events.extend(self.master.poll(Instant::now()).unwrap());
        events
    }

    /// Polls until the predicate holds and stops there, without the extra poll `until` adds.
    fn poll_until(&mut self, mut predicate: impl FnMut(&Master<E>) -> bool) -> Vec<MasterEvent> {
        let started = Instant::now();
        let mut events = Vec::new();
        while !predicate(&self.master) {
            events.extend(self.master.poll(Instant::now()).unwrap());
            assert!(
                started.elapsed() < TEST_TIMEOUT,
                "master remained in {:?}; events: {events:?}",
                self.master.state(),
            );
            thread::sleep(POLL_INTERVAL);
        }
        events
    }

    /// Polls with the master clock held at `now` until an event matches, so deadlines cannot fire
    /// while the test waits for a worker process to react.
    fn until_event_at(
        &mut self,
        now: Instant,
        wanted: impl Fn(&MasterEvent) -> bool,
    ) -> Vec<MasterEvent> {
        let started = Instant::now();
        let mut events = Vec::new();
        while !events.iter().any(&wanted) {
            events.extend(self.master.poll(now).unwrap());
            assert!(
                started.elapsed() < TEST_TIMEOUT,
                "master remained in {:?}; events: {events:?}",
                self.master.state(),
            );
            thread::sleep(POLL_INTERVAL);
        }
        events
    }

    /// Crashes the active worker and waits until its launcher is a zombie, so the master first
    /// sees the exit rather than the closing channel that precedes it.
    fn crash_active(&mut self) {
        let launcher = self
            .master
            .worker_process_group_id(WorkerRole::Active)
            .unwrap();
        self.master.notify_active(b"crash").unwrap();
        wait_for_launcher_exit(launcher);
    }

    /// Waits until the candidate, which aborts on the activation the master just sent, has
    /// exited. The master has not polled since sending it, so it first sees the exit rather than
    /// the closing channel that precedes it.
    fn candidate_crashes_on_activation(&mut self) -> Vec<MasterEvent> {
        let events = self.poll_until(|master| master.state() == MasterState::ActivatingCandidate);
        wait_for_launcher_exit(
            self.master
                .worker_process_group_id(WorkerRole::Candidate)
                .unwrap(),
        );
        events
    }

    /// Crashes the old active worker while the candidate activates and returns the events that
    /// report its exit. The master clock stays before the activation deadline meanwhile.
    fn crash_active_while_activating(&mut self) -> Vec<MasterEvent> {
        let held = self
            .master
            .next_deadline()
            .unwrap()
            .checked_sub(POLL_INTERVAL)
            .unwrap();
        self.crash_active();
        let events = self.until_event_at(held, |event| {
            matches!(
                event,
                MasterEvent::WorkerExited {
                    role: WorkerRole::Active,
                    ..
                }
            )
        });
        assert_eq!(self.master.state(), MasterState::ActivatingCandidate);
        events
    }

    fn shutdown(mut self) -> Vec<MasterEvent> {
        assert!(matches!(
            self.master.shutdown(Instant::now()).unwrap(),
            ShutdownProgress::Pending { .. }
        ));
        self.until(|master| master.state() == MasterState::Stopped)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    Duplicate(WorkerRole),
    Send(WorkerRole, ControlPhase),
    Prepare(WorkerRole, ControlPhase, PreparationStep),
}

impl FaultPoint {
    const fn action(self) -> ActionKind {
        match self {
            Self::Duplicate(_) => ActionKind::DuplicateListeners,
            Self::Send(_, phase) => ActionKind::Send(phase),
            Self::Prepare(..) => panic!("preparation fault has no action kind"),
        }
    }
}

#[derive(Debug, Default)]
struct FaultExecutor {
    system: SystemActionExecutor,
    fault: Option<FaultPoint>,
}

impl FaultExecutor {
    fn fail(&mut self, fault: FaultPoint) {
        self.fault = Some(fault);
    }

    fn should_fail(&mut self, fault: FaultPoint) -> bool {
        if self.fault == Some(fault) {
            self.fault = None;
            true
        } else {
            false
        }
    }
}

impl ActionExecutor for FaultExecutor {
    fn prepare(
        &mut self,
        role: WorkerRole,
        phase: ControlPhase,
        step: PreparationStep,
    ) -> Result<(), PreparationError> {
        if self.should_fail(FaultPoint::Prepare(role, phase, step)) {
            return Err(PreparationError::Injected);
        }
        Ok(())
    }

    fn duplicate_listeners(
        &mut self,
        role: WorkerRole,
        listeners: &StableListeners,
    ) -> Result<Vec<OwnedFd>, ActionError> {
        if self.should_fail(FaultPoint::Duplicate(role)) {
            return Err(ActionError::Injected);
        }
        self.system.duplicate_listeners(role, listeners)
    }

    fn send(
        &mut self,
        role: WorkerRole,
        process: &mut WorkerProcess,
        phase: ControlPhase,
        payload: &[u8],
        descriptors: &[OwnedFd],
    ) -> Result<(), ActionError> {
        if self.should_fail(FaultPoint::Send(role, phase)) {
            return Err(ActionError::Injected);
        }
        self.system.send(role, process, phase, payload, descriptors)
    }
}

#[derive(Debug, Default)]
struct FailingFactory {
    calls: usize,
}

impl oxiroute_supervisor_master::WorkerFactory for FailingFactory {
    type Command = WorkerCommand;
    type Error = io::Error;

    fn spawn(
        &mut self,
        _command: Self::Command,
        _identity: WorkerIdentity,
    ) -> Result<WorkerProcess, Self::Error> {
        self.calls += 1;
        Err(io::Error::other("injected spawn failure"))
    }
}

/// The sockets a replacement keeps from the launch set and the ones it adds.
struct NextSet {
    keep_tcp: bool,
    keep_unix: bool,
    add_tcp: Option<TcpListener>,
}

impl KeptListeners {
    /// Builds the next listener set: slot 1 is the launch TCP socket, slot 2 the launch Unix
    /// socket and slot 3 an added TCP socket. Kept sockets are duplicates of the launch ones.
    fn next_set(
        &self,
        tcp_address: SocketAddr,
        unix_path: &Path,
        next: NextSet,
    ) -> StableListeners {
        let mut slots = Vec::new();
        let mut originals = Vec::new();
        if next.keep_tcp {
            slots.push(DescriptorSlot {
                id: SlotId(1),
                role: DescriptorRole::Traffic(String::from("tcp")),
                kind: DescriptorKind::TcpListener,
                bind: Some(BindIdentity::Tcp(tcp_address)),
                mode: None,
            });
            originals.push(OwnedFd::from(self.tcp.try_clone().unwrap()));
        }
        if next.keep_unix {
            slots.push(DescriptorSlot {
                id: SlotId(2),
                role: DescriptorRole::Traffic(String::from("unix")),
                kind: DescriptorKind::UnixListener,
                bind: Some(BindIdentity::UnixPath(unix_path.to_path_buf())),
                mode: None,
            });
            originals.push(OwnedFd::from(self.unix.try_clone().unwrap()));
        }
        if let Some(added) = next.add_tcp {
            slots.push(DescriptorSlot {
                id: SlotId(3),
                role: DescriptorRole::Traffic(String::from("added")),
                kind: DescriptorKind::TcpListener,
                bind: Some(BindIdentity::Tcp(added.local_addr().unwrap())),
                mode: None,
            });
            originals.push(OwnedFd::from(added));
        }
        StableListeners::new(
            DescriptorManifest::new(slots).unwrap(),
            originals,
            Arc::clone(&self.path_guard),
        )
        .unwrap()
    }
}

#[derive(Debug)]
struct UnixPathGuard(PathBuf);

impl Drop for UnixPathGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
