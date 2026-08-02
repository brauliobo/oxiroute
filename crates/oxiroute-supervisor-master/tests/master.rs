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
    path::PathBuf,
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot,
    InstanceToken, SlotId,
};
use oxiroute_supervisor_master::{
    ActionError, ActionExecutor, ActionKind, CONTROL_PROTOCOL_VERSION, ControlPhase, FailurePhase,
    Master, MasterConfig, MasterError, MasterEvent, MasterState, PreparationError, PreparationStep,
    ShutdownProgress, StableListeners, SystemActionExecutor, WorkerInput, WorkerRole, WorkerState,
};
use oxiroute_supervisor_process::{WorkerCommand, WorkerIdentity, WorkerProcess, WorkerSpawner};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Harness<E: ActionExecutor = SystemActionExecutor> {
    _temporary: tempfile::TempDir,
    listener_targets: Vec<PathBuf>,
    tcp_address: SocketAddr,
    unix_path: PathBuf,
    factory: WorkerSpawner,
    master: Master<E>,
}

impl Harness<SystemActionExecutor> {
    fn launch(active_behavior: &str) -> Self {
        Self::launch_with_executor(active_behavior, SystemActionExecutor)
    }
}

impl<E: ActionExecutor> Harness<E> {
    fn launch_with_executor(active_behavior: &str, executor: E) -> Self {
        let (temporary, listeners, listener_targets, tcp_address, unix_path) = listeners();
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
            worker("a", 1, active_behavior),
            Instant::now(),
        )
        .unwrap();
        let mut harness = Self {
            _temporary: temporary,
            listener_targets,
            tcp_address,
            unix_path,
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

    fn until(&mut self, mut predicate: impl FnMut(&Master<E>) -> bool) -> Vec<MasterEvent> {
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
        events.extend(self.master.poll(Instant::now()).unwrap());
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

#[test]
fn replaces_a_with_b_using_the_same_tcp_and_unix_listener_identities() {
    let mut harness = Harness::launch("normal");
    let manifest = harness.master.listener_manifest().clone();
    harness.replace("normal", 2);
    assert_eq!(harness.master.state(), MasterState::AdoptingCandidate);
    assert_served_by(&harness, 1);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert_eq!(harness.master.listener_manifest(), &manifest);
    assert_served_by(&harness, 2);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCommitted { active, retired }
            if active.as_str() == "b" && retired.as_str() == "a"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCompleted { active } if active.as_str() == "b"
    )));
    let events = harness.shutdown();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
}

#[test]
fn initial_adoption_and_activation_fail_closed() {
    for (behavior, expected) in [
        ("reject-adopt", FailurePhase::ListenerAdoption),
        ("reject-activate", FailurePhase::Activation),
    ] {
        let (_temporary, listeners, _, _, _) = listeners();
        let mut factory = factory();
        let mut master = Master::launch(
            config(),
            listeners,
            &mut factory,
            worker("a", 1, behavior),
            Instant::now(),
        )
        .unwrap();
        let started = Instant::now();
        let mut events = Vec::new();
        while master.state() != MasterState::Failed {
            events.extend(master.poll(Instant::now()).unwrap());
            assert!(started.elapsed() < TEST_TIMEOUT);
            thread::sleep(POLL_INTERVAL);
        }
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::Failed { phase } if *phase == expected
        )));
    }
}

#[test]
fn rolls_back_candidate_rejections_during_adoption_and_activation() {
    for behavior in ["reject-adopt", "reject-activate"] {
        let mut harness = Harness::launch("normal");
        harness.replace(behavior, 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|id| id.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::RollbackStarted { candidate, .. } if candidate.as_str() == "b"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::RollbackCompleted { active } if active.as_str() == "a"
        )));
        harness.shutdown();
    }
}

#[test]
fn rolls_back_when_active_rejects_quiesce() {
    let mut harness = Harness::launch("reject-quiesce");
    harness.replace("normal", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "a")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Quiesce,
            ..
        }
    )));
    harness.shutdown();
}

#[test]
fn retired_drain_failure_keeps_the_committed_candidate_active() {
    let mut harness = Harness::launch("reject-drain");
    harness.replace("normal", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RetiredFailed {
            phase: FailurePhase::Drain
        }
    )));
    harness.shutdown();
}

#[test]
fn failed_reactivation_fails_closed() {
    let mut harness = Harness::launch("reject-reactivate");
    harness.replace("reject-activate", 2);
    let events = harness.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::Failed {
            phase: FailurePhase::Reactivation
        }
    )));
}

#[test]
fn stale_acknowledgement_is_observed_without_advancing_the_phase() {
    let mut harness = Harness::launch("normal");
    harness.replace("stale-adopt", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "b")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::StaleAcknowledgement { request_id: 2, .. }
    )));
    harness.shutdown();
}

#[test]
fn candidate_crash_rolls_back_and_active_crash_fails_closed() {
    let mut candidate = Harness::launch("normal");
    candidate.replace("crash-activate", 2);
    let events = candidate.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|id| id.as_str() == "a")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Crash,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Candidate,
            ..
        }
    )));
    candidate.shutdown();

    let mut active = Harness::launch("crash-after-activate");
    let events = active.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::Failed {
            phase: FailurePhase::Crash
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Active,
            ..
        }
    )));
}

#[test]
fn failed_candidates_do_not_leak_parent_descriptors() {
    let mut harness = Harness::launch("normal");
    let baseline = matching_descriptor_count(&harness.listener_targets);
    assert_eq!(baseline, 2);
    for generation in 2..=5 {
        harness.replace("reject-adopt", generation);
        harness.until(|master| master.state() == MasterState::Running);
        assert_eq!(
            matching_descriptor_count(&harness.listener_targets),
            baseline
        );
    }
    harness.shutdown();
}

#[test]
fn shutdown_deadline_kills_and_reaps_an_uncooperative_worker() {
    let harness = Harness::launch("ignore-shutdown");
    let worker_pid = harness.master.worker_id(WorkerRole::Active).unwrap();
    let launcher_pid = harness
        .master
        .worker_process_group_id(WorkerRole::Active)
        .unwrap();
    let mut harness = harness;
    let now = Instant::now();
    assert_eq!(
        harness.master.shutdown(now).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: false
        }
    );
    let deadline = harness.master.next_deadline().unwrap();
    let started = Instant::now();
    let mut events = harness.master.poll(deadline + POLL_INTERVAL).unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(matches!(
        harness.master.state(),
        MasterState::ShuttingDown | MasterState::Stopped
    ));
    if harness.master.state() == MasterState::ShuttingDown {
        assert_eq!(
            harness.master.worker_state(WorkerRole::Active),
            Some(WorkerState::Terminating { forced: true })
        );
        assert_eq!(
            harness.master.shutdown_progress(),
            ShutdownProgress::Pending {
                remaining: 1,
                forced: true
            }
        );
    }
    events.extend(harness.until(|master| master.state() == MasterState::Stopped));
    assert_eq!(
        harness.master.shutdown_progress(),
        ShutdownProgress::Complete { forced: true }
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
    wait_process_gone(worker_pid);
    wait_process_gone(launcher_pid);
}

#[test]
fn rejected_shutdown_is_killed_and_reaped() {
    let events = Harness::launch("reject-shutdown").shutdown();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
}

#[test]
fn deadline_expiry_precedes_matching_acknowledgement() {
    let mut harness = Harness::launch("delay-quiesce");
    harness.replace("normal", 2);
    harness.until(|master| master.state() == MasterState::Quiescing);
    let expired = harness.master.next_deadline().unwrap();
    thread::sleep(Duration::from_millis(100));
    let mut events = harness.master.poll(expired).unwrap();
    assert_ne!(harness.master.state(), MasterState::ActivatingCandidate);
    events.extend(harness.until(|master| master.state() == MasterState::Running));
    assert_eq!(harness.master.active_instance().unwrap().as_str(), "a");
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::StaleAcknowledgement {
            phase: ControlPhase::Quiesce,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::ReplacementCommitted { .. }))
    );
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
fn candidate_activation_ack_precedes_collected_old_active_exit() {
    let mut harness = Harness::launch("crash-after-quiesce");
    harness.replace("normal", 2);
    let mut events = harness.until(|master| master.state() == MasterState::ActivatingCandidate);
    thread::sleep(Duration::from_millis(100));
    events.extend(harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ReplacementCommitted { active, .. } if active.as_str() == "b"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MasterEvent::FailClosed { .. }))
    );
    assert_served_by(&harness, 2);
    harness.shutdown();
}

#[test]
fn channel_disconnect_waits_for_exit_and_rolls_back_consistently() {
    let mut harness = Harness::launch("normal");
    harness.replace("disconnect-activate", 2);
    let events = harness.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "a")
    });
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerDisconnected {
            role: WorkerRole::Candidate,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::RollbackStarted {
            phase: FailurePhase::Disconnected,
            ..
        }
    )));
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
fn initial_duplicate_and_send_faults_fail_closed_without_synchronous_reap() {
    for fault in [
        FaultPoint::Duplicate(WorkerRole::Active),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::AdoptListeners),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::Activate),
    ] {
        let mut executor = FaultExecutor::default();
        executor.fail(fault);
        let (_temporary, listeners, _, _, _) = listeners();
        let mut factory = factory();
        let mut master = Master::launch_with_executor(
            config(),
            listeners,
            executor,
            &mut factory,
            worker("a", 1, "normal"),
            Instant::now(),
        )
        .unwrap();
        let started = Instant::now();
        let events = poll_until(&mut master, |master| master.state() == MasterState::Failed);
        assert!(started.elapsed() < TEST_TIMEOUT);
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
    }
}

#[test]
fn candidate_duplicate_and_precommit_send_faults_compensate_to_active() {
    for fault in [
        FaultPoint::Duplicate(WorkerRole::Candidate),
        FaultPoint::Send(WorkerRole::Candidate, ControlPhase::AdoptListeners),
        FaultPoint::Send(WorkerRole::Active, ControlPhase::Quiesce),
        FaultPoint::Send(WorkerRole::Candidate, ControlPhase::Activate),
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness.master.action_executor_mut().fail(fault);
        harness.replace("normal", 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, MasterEvent::RollbackCompleted { .. }))
        );
        assert_served_by(&harness, 1);
        harness.shutdown();
    }
}

#[test]
fn retired_send_faults_keep_candidate_and_track_old_until_reaped() {
    for fault in [
        FaultPoint::Send(WorkerRole::Retired, ControlPhase::Drain),
        FaultPoint::Send(WorkerRole::Retired, ControlPhase::Shutdown),
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness.master.action_executor_mut().fail(fault);
        harness.replace("normal", 2);
        let mut saw_terminating = false;
        let events = harness.until(|master| {
            saw_terminating |= matches!(
                master.worker_state(WorkerRole::Retired),
                Some(WorkerState::Terminating { forced: true })
            );
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "b")
        });
        assert!(saw_terminating);
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::ActionFailed { action, .. }
                if *action == fault.action()
        )));
        assert_served_by(&harness, 2);
        harness.shutdown();
    }
}

#[test]
fn reactivation_send_fault_fails_closed_and_shutdown_send_fault_is_forced_pending() {
    let mut rollback = Harness::launch_with_executor("normal", FaultExecutor::default());
    rollback.master.action_executor_mut().fail(FaultPoint::Send(
        WorkerRole::Active,
        ControlPhase::Reactivate,
    ));
    rollback.replace("reject-activate", 2);
    let events = rollback.until(|master| master.state() == MasterState::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::ActionFailed {
            action: ActionKind::Send(ControlPhase::Reactivate),
            ..
        }
    )));

    let mut shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Send(WorkerRole::Active, ControlPhase::Shutdown));
    let started = Instant::now();
    assert_eq!(
        shutdown.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: true
        }
    );
    assert!(started.elapsed() < Duration::from_millis(100));
    shutdown.until(|master| master.state() == MasterState::Stopped);
}

#[test]
fn candidate_adoption_preparation_faults_are_compensated_without_stranding() {
    for step in [
        PreparationStep::RequestId,
        PreparationStep::Deadline,
        PreparationStep::Encoding,
        PreparationStep::Allocation,
    ] {
        let mut harness = Harness::launch_with_executor("normal", FaultExecutor::default());
        harness
            .master
            .action_executor_mut()
            .fail(FaultPoint::Prepare(
                WorkerRole::Candidate,
                ControlPhase::AdoptListeners,
                step,
            ));
        harness.replace("normal", 2);
        let events = harness.until(|master| {
            master.state() == MasterState::Running
                && master
                    .active_instance()
                    .is_some_and(|instance| instance.as_str() == "a")
        });
        assert!(events.iter().any(|event| matches!(
            event,
            MasterEvent::PreparationFailed {
                role: WorkerRole::Candidate,
                phase: ControlPhase::AdoptListeners,
                step: actual,
            } if *actual == step
        )));
        assert_eq!(harness.master.worker_state(WorkerRole::Candidate), None);
        assert_eq!(harness.master.next_deadline(), None);
        assert_served_by(&harness, 1);
        harness.shutdown();
    }
}

#[test]
fn preparation_faults_after_lifecycle_mutation_compensate_or_fail_closed() {
    let mut quiesce = Harness::launch_with_executor("delay-reactivate", FaultExecutor::default());
    quiesce
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Quiesce,
            PreparationStep::Deadline,
        ));
    quiesce.replace("normal", 2);
    let mut events = quiesce.until(|master| master.state() == MasterState::RollingBack);
    assert!(quiesce.master.next_deadline().is_some());
    events.extend(quiesce.until(|master| master.state() == MasterState::Running));
    assert_preparation_failure(&events, ControlPhase::Quiesce, PreparationStep::Deadline);
    assert_served_by(&quiesce, 1);
    quiesce.shutdown();

    let mut activate = Harness::launch_with_executor("normal", FaultExecutor::default());
    activate
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Candidate,
            ControlPhase::Activate,
            PreparationStep::Encoding,
        ));
    activate.replace("normal", 2);
    let events = activate.until(|master| master.state() == MasterState::Running);
    assert_preparation_failure(&events, ControlPhase::Activate, PreparationStep::Encoding);
    assert_eq!(activate.master.active_instance().unwrap().as_str(), "a");
    activate.shutdown();

    let mut drain = Harness::launch_with_executor("normal", FaultExecutor::default());
    drain.master.action_executor_mut().fail(FaultPoint::Prepare(
        WorkerRole::Retired,
        ControlPhase::Drain,
        PreparationStep::Allocation,
    ));
    drain.replace("normal", 2);
    let events = drain.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    });
    assert_preparation_failure(&events, ControlPhase::Drain, PreparationStep::Allocation);
    assert_served_by(&drain, 2);
    drain.shutdown();

    let mut retired_shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    retired_shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Retired,
            ControlPhase::Shutdown,
            PreparationStep::RequestId,
        ));
    retired_shutdown.replace("normal", 2);
    let events = retired_shutdown.until(|master| {
        master.state() == MasterState::Running
            && master
                .active_instance()
                .is_some_and(|instance| instance.as_str() == "b")
    });
    assert_preparation_failure(&events, ControlPhase::Shutdown, PreparationStep::RequestId);
    retired_shutdown.shutdown();

    let mut reactivate = Harness::launch_with_executor("normal", FaultExecutor::default());
    reactivate
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Reactivate,
            PreparationStep::Deadline,
        ));
    reactivate.replace("reject-activate", 2);
    let events = reactivate.until(|master| master.state() == MasterState::Failed);
    assert_preparation_failure(&events, ControlPhase::Reactivate, PreparationStep::Deadline);

    let mut shutdown = Harness::launch_with_executor("normal", FaultExecutor::default());
    shutdown
        .master
        .action_executor_mut()
        .fail(FaultPoint::Prepare(
            WorkerRole::Active,
            ControlPhase::Shutdown,
            PreparationStep::Encoding,
        ));
    assert_eq!(
        shutdown.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: true,
        }
    );
    let events = shutdown.until(|master| master.state() == MasterState::Stopped);
    assert_preparation_failure(&events, ControlPhase::Shutdown, PreparationStep::Encoding);
}

#[test]
fn shutdown_consumes_deferred_active_exit_and_completes() {
    let mut harness = Harness::launch("crash-after-quiesce");
    harness.replace("delay-activate", 2);
    harness.until(|master| master.state() == MasterState::ActivatingCandidate);
    thread::sleep(Duration::from_millis(75));
    let events = harness.master.poll(Instant::now()).unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::WorkerExited {
            role: WorkerRole::Active,
            ..
        }
    )));
    assert_eq!(harness.master.state(), MasterState::ActivatingCandidate);

    assert_eq!(
        harness.master.shutdown(Instant::now()).unwrap(),
        ShutdownProgress::Pending {
            remaining: 1,
            forced: false,
        }
    );
    let events = harness.until(|master| master.state() == MasterState::Stopped);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, MasterEvent::ShutdownCompleted { .. }))
    );
    assert_eq!(
        harness.master.shutdown_progress(),
        ShutdownProgress::Complete { forced: false }
    );
}

#[test]
fn begin_is_validated_before_spawn_and_spawn_failure_leaves_active_unchanged() {
    let mut harness = Harness::launch("normal");
    let mut failing = FailingFactory::default();
    assert!(
        harness
            .master
            .replace(&mut failing, worker("stale", 1, "normal"), Instant::now())
            .is_err()
    );
    assert_eq!(failing.calls, 0);

    assert!(
        harness
            .master
            .replace(&mut failing, worker("b", 2, "normal"), Instant::now())
            .is_err()
    );
    assert_eq!(failing.calls, 1);
    let events = harness.master.poll(Instant::now()).unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::SpawnFailed { instance_id } if instance_id.as_str() == "b"
    )));
    assert_eq!(harness.master.state(), MasterState::Running);
    assert_served_by(&harness, 1);
    harness.shutdown();
}

#[test]
fn stable_listener_originals_are_cloexec_and_not_inherited() {
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = fs::read_link(format!("/proc/self/fd/{}", tcp.as_raw_fd())).unwrap();
    let flags = fcntl_getfd(&tcp).unwrap();
    fcntl_setfd(&tcp, flags - FdFlags::CLOEXEC).unwrap();
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(1),
        role: DescriptorRole::Traffic(String::from("tcp")),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(tcp.local_addr().unwrap())),
        mode: None,
    }])
    .unwrap();
    let _listeners =
        StableListeners::new(manifest, vec![OwnedFd::from(tcp)], Arc::new(())).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_master-worker-fixture"))
        .arg("probe-fd")
        .arg(target.as_os_str())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn configuration_rejects_zero_and_unbounded_timeouts() {
    assert!(
        MasterConfig::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .is_err()
    );
    assert!(
        MasterConfig::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(3_601),
        )
        .is_err()
    );
}

#[test]
fn version_two_master_rejects_a_version_one_worker_before_spawn() {
    let listeners = StableListeners::new(
        DescriptorManifest::new(Vec::new()).unwrap(),
        Vec::new(),
        Arc::new(()),
    )
    .unwrap();
    let mut input = worker("legacy", 1, "normal");
    input.identity.protocol = 1;
    let mut factory = FailingFactory::default();

    assert!(matches!(
        Master::launch(config(), listeners, &mut factory, input, Instant::now()),
        Err(MasterError::ProtocolVersion {
            expected: 2,
            actual: 1
        })
    ));
    assert_eq!(factory.calls, 0, "legacy worker reached the spawn boundary");
}

fn listeners() -> (
    tempfile::TempDir,
    StableListeners,
    Vec<PathBuf>,
    SocketAddr,
    PathBuf,
) {
    let temporary = tempfile::tempdir().unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_address = tcp.local_addr().unwrap();
    let mut unix_bytes = temporary.path().as_os_str().as_bytes().to_vec();
    unix_bytes.extend_from_slice(b"/master-\xff.sock");
    let unix_path = PathBuf::from(OsString::from_vec(unix_bytes));
    let unix = UnixListener::bind(&unix_path).unwrap();
    let targets = [&tcp as &dyn AsRawFd, &unix as &dyn AsRawFd]
        .into_iter()
        .map(|listener| fs::read_link(format!("/proc/self/fd/{}", listener.as_raw_fd())).unwrap())
        .collect();
    let manifest = DescriptorManifest::new(vec![
        DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Traffic(String::from("tcp")),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(tcp_address)),
            mode: None,
        },
        DescriptorSlot {
            id: SlotId(2),
            role: DescriptorRole::Traffic(String::from("unix")),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(unix_path.clone())),
            mode: None,
        },
    ])
    .unwrap();
    let originals = vec![OwnedFd::from(tcp), OwnedFd::from(unix)];
    let listeners = StableListeners::new(
        manifest,
        originals,
        Arc::new(UnixPathGuard(unix_path.clone())),
    )
    .unwrap();
    (temporary, listeners, targets, tcp_address, unix_path)
}

#[derive(Debug)]
struct UnixPathGuard(PathBuf);

impl Drop for UnixPathGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn config() -> MasterConfig {
    MasterConfig::new(
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
        Duration::from_millis(300),
    )
    .unwrap()
}

fn factory() -> WorkerSpawner {
    WorkerSpawner::new(
        env!("CARGO_BIN_EXE_master-launcher-fixture"),
        Duration::from_secs(2),
    )
    .unwrap()
}

fn worker(name: &str, generation: u64, behavior: &str) -> WorkerInput<WorkerCommand> {
    let token = [u8::try_from(generation).unwrap(); 16];
    let mut encoded_token = String::with_capacity(32);
    for byte in token {
        write!(encoded_token, "{byte:02x}").unwrap();
    }
    WorkerInput {
        instance_id: InstanceId::new(name).unwrap(),
        identity: WorkerIdentity {
            instance: InstanceToken(token),
            generation: GenerationId(generation),
            protocol: CONTROL_PROTOCOL_VERSION,
        },
        command: WorkerCommand::new(env!("CARGO_BIN_EXE_master-worker-fixture"))
            .unwrap()
            .arg(behavior)
            .arg(generation.to_string())
            .arg(encoded_token),
    }
}

fn matching_descriptor_count(targets: &[PathBuf]) -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| targets.contains(target))
        .count()
}

fn poll_until<E: ActionExecutor>(
    master: &mut Master<E>,
    mut predicate: impl FnMut(&Master<E>) -> bool,
) -> Vec<MasterEvent> {
    let started = Instant::now();
    let mut events = Vec::new();
    while !predicate(master) {
        events.extend(master.poll(Instant::now()).unwrap());
        assert!(started.elapsed() < TEST_TIMEOUT);
        thread::sleep(POLL_INTERVAL);
    }
    events.extend(master.poll(Instant::now()).unwrap());
    events
}

fn assert_served_by<E: ActionExecutor>(harness: &Harness<E>, generation: u64) {
    for _ in 0..5 {
        assert_eq!(probe_tcp(harness.tcp_address), generation);
        assert_eq!(probe_unix(&harness.unix_path), generation);
    }
}

fn assert_preparation_failure(events: &[MasterEvent], phase: ControlPhase, step: PreparationStep) {
    assert!(events.iter().any(|event| matches!(
        event,
        MasterEvent::PreparationFailed {
            phase: actual_phase,
            step: actual_step,
            ..
        } if *actual_phase == phase && *actual_step == step
    )));
}

fn probe_tcp(address: SocketAddr) -> u64 {
    let started = Instant::now();
    loop {
        if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(50)) {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut response = [0_u8; 8];
            if stream.read_exact(&mut response).is_ok() {
                return u64::from_be_bytes(response);
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

fn probe_unix(path: &PathBuf) -> u64 {
    let started = Instant::now();
    loop {
        if let Ok(mut stream) = UnixStream::connect(path) {
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut response = [0_u8; 8];
            if stream.read_exact(&mut response).is_ok() {
                return u64::from_be_bytes(response);
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

fn wait_process_gone(pid: u32) {
    let path = PathBuf::from(format!("/proc/{pid}"));
    let started = Instant::now();
    while path.exists() && started.elapsed() < Duration::from_secs(2) {
        thread::sleep(POLL_INTERVAL);
    }
    assert!(!path.exists(), "process {pid} was not reaped");
}
