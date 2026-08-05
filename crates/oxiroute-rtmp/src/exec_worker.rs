use std::{
    collections::HashMap,
    io::{Read, Write},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{
    ExecMode, ExecNetworkPolicy, ExecProfile, ExecTrigger, MediaEvent, MediaEventKind, SessionId,
    StreamKey,
};

const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const OUTPUT_READ_BUFFER_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecWorkerCorrelation {
    pub service: String,
    pub application: String,
    pub stream: String,
    pub session_id: SessionId,
    pub profile: String,
}

impl ExecWorkerCorrelation {
    fn new(service: &str, key: &StreamKey, session_id: SessionId, profile: &ExecProfile) -> Self {
        Self {
            service: service.to_owned(),
            application: key.application.clone(),
            stream: key.name.clone(),
            session_id,
            profile: profile.name().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecWorkerPhase {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecWorkerFailure {
    SpawnFailed,
    NetworkIsolationUnavailable,
    FilesystemPolicyUnavailable,
    InputWriteFailed,
    OutputLimit,
    Timeout,
    ProcessExited,
    RespawnLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecWorkerStatus {
    pub correlation: ExecWorkerCorrelation,
    pub phase: ExecWorkerPhase,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub stdin_bytes: usize,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub dropped_events: u64,
    pub respawns: usize,
    pub output_truncated: bool,
    pub failure: Option<ExecWorkerFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecEnqueueResult {
    Queued,
    Filtered,
    Dropped,
    Inactive,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExecWorkerStartError {
    #[error("exec worker process limit reached")]
    ProcessLimit,
    #[error("exec worker thread could not be started")]
    ThreadStart,
}

enum WorkerCommand {
    Media(Vec<u8>),
    Stop,
}

struct ExecWorkerInner {
    sender: SyncSender<WorkerCommand>,
    state: Mutex<ExecWorkerStatus>,
    mode: ExecMode,
    max_queue_bytes: usize,
    queued_bytes: AtomicUsize,
    pid: AtomicU32,
    stop_requested: AtomicBool,
    join: Mutex<Option<JoinHandle<()>>>,
}

pub(crate) struct ExecWorker {
    inner: Arc<ExecWorkerInner>,
}

impl ExecWorker {
    fn start(
        profile: ExecProfile,
        correlation: ExecWorkerCorrelation,
        lease: ExecProcessLease,
    ) -> Result<Self, ExecWorkerStartError> {
        let limits = profile.limits();
        let (sender, receiver) = mpsc::sync_channel(limits.max_queue_messages);
        let state = ExecWorkerStatus {
            correlation,
            phase: ExecWorkerPhase::Starting,
            pid: None,
            exit_code: None,
            stdin_bytes: 0,
            stdout_bytes: 0,
            stderr_bytes: 0,
            dropped_events: 0,
            respawns: 0,
            output_truncated: false,
            failure: None,
        };
        let inner = Arc::new(ExecWorkerInner {
            sender,
            state: Mutex::new(state),
            mode: profile.mode(),
            max_queue_bytes: limits.max_queue_bytes,
            queued_bytes: AtomicUsize::new(0),
            pid: AtomicU32::new(0),
            stop_requested: AtomicBool::new(false),
            join: Mutex::new(None),
        });
        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name("oxiroute-rtmp-exec".into())
            .spawn(move || run_worker(&profile, worker_inner.as_ref(), &receiver, lease))
            .map_err(|_| ExecWorkerStartError::ThreadStart)?;
        *inner
            .join
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Ok(Self { inner })
    }

    pub(crate) fn status(&self) -> ExecWorkerStatus {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn try_enqueue(&self, event: &MediaEvent) -> ExecEnqueueResult {
        if self.inner.mode != ExecMode::Transcode {
            return ExecEnqueueResult::Filtered;
        }
        let status = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.phase != ExecWorkerPhase::Starting && status.phase != ExecWorkerPhase::Running {
            return ExecEnqueueResult::Inactive;
        }
        drop(status);

        let frame = encode_media_event(event);
        self.try_enqueue_frame(frame)
    }

    fn try_enqueue_frame(&self, frame: Vec<u8>) -> ExecEnqueueResult {
        let length = frame.len();
        let reserved = self
            .inner
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                queued
                    .checked_add(length)
                    .filter(|total| *total <= self.inner.max_queue_bytes)
            })
            .is_ok();
        if !reserved {
            let mut status = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            status.dropped_events = status.dropped_events.saturating_add(1);
            return ExecEnqueueResult::Dropped;
        }
        let result = self.inner.sender.try_send(WorkerCommand::Media(frame));
        match result {
            Ok(()) => ExecEnqueueResult::Queued,
            Err(TrySendError::Full(WorkerCommand::Media(frame))) => {
                self.inner
                    .queued_bytes
                    .fetch_sub(frame.len(), Ordering::AcqRel);
                let mut status = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                status.dropped_events = status.dropped_events.saturating_add(1);
                ExecEnqueueResult::Dropped
            }
            Err(TrySendError::Disconnected(WorkerCommand::Media(frame))) => {
                self.inner
                    .queued_bytes
                    .fetch_sub(frame.len(), Ordering::AcqRel);
                ExecEnqueueResult::Inactive
            }
            Err(
                TrySendError::Full(WorkerCommand::Stop)
                | TrySendError::Disconnected(WorkerCommand::Stop),
            ) => ExecEnqueueResult::Inactive,
        }
    }

    fn stop(&self) {
        self.inner.stop_requested.store(true, Ordering::Release);
        {
            let mut status = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(
                status.phase,
                ExecWorkerPhase::Starting | ExecWorkerPhase::Running
            ) {
                status.phase = ExecWorkerPhase::Stopping;
            }
        }
        kill_current_process(&self.inner.pid);
        let _ = self.inner.sender.try_send(WorkerCommand::Stop);
    }
}

impl Drop for ExecWorker {
    fn drop(&mut self) {
        self.stop();
        let join = self
            .inner
            .join
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(join) = join {
            let _ = join.join();
        }
    }
}

pub(crate) struct ExecProfileSet {
    profiles: Arc<[ExecProfile]>,
    active: Arc<Mutex<HashMap<String, usize>>>,
    oneshots: Mutex<Vec<ExecWorker>>,
}

impl ExecProfileSet {
    pub(crate) fn new(profiles: impl IntoIterator<Item = ExecProfile>) -> Option<Arc<Self>> {
        let profiles: Vec<_> = profiles.into_iter().collect();
        (!profiles.is_empty()).then(|| {
            Arc::new(Self {
                profiles: Arc::from(profiles),
                active: Arc::new(Mutex::new(HashMap::new())),
                oneshots: Mutex::new(Vec::new()),
            })
        })
    }

    pub(crate) fn start_publisher(
        &self,
        service: &str,
        key: &StreamKey,
        session_id: SessionId,
    ) -> Vec<ExecWorker> {
        self.reap_oneshots();
        self.start_matching(service, key, session_id, ExecTrigger::Publisher)
    }

    pub(crate) fn start_publish_done(&self, service: &str, key: &StreamKey, session_id: SessionId) {
        self.reap_oneshots();
        let workers = self.start_matching(service, key, session_id, ExecTrigger::PublishDone);
        if workers.is_empty() {
            return;
        }
        self.oneshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(workers);
    }

    fn start_matching(
        &self,
        service: &str,
        key: &StreamKey,
        session_id: SessionId,
        trigger: ExecTrigger,
    ) -> Vec<ExecWorker> {
        let mut workers = Vec::new();
        for profile in self
            .profiles
            .iter()
            .filter(|profile| profile.trigger() == trigger)
        {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let count = active.entry(profile.name().to_owned()).or_default();
            if *count >= profile.limits().max_processes {
                continue;
            }
            *count += 1;
            drop(active);
            let lease = ExecProcessLease {
                active: Arc::clone(&self.active),
                profile: profile.name().to_owned(),
            };
            let correlation = ExecWorkerCorrelation::new(service, key, session_id, profile);
            if let Ok(worker) = ExecWorker::start(profile.clone(), correlation, lease) {
                workers.push(worker);
            }
        }
        workers
    }

    fn reap_oneshots(&self) {
        let mut finished = Vec::new();
        let mut oneshots = self
            .oneshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut retained = Vec::with_capacity(oneshots.len());
        for worker in oneshots.drain(..) {
            if matches!(
                worker.status().phase,
                ExecWorkerPhase::Exited | ExecWorkerPhase::Failed
            ) {
                finished.push(worker);
            } else {
                retained.push(worker);
            }
        }
        *oneshots = retained;
        drop(oneshots);
        drop(finished);
    }
}

struct ExecProcessLease {
    active: Arc<Mutex<HashMap<String, usize>>>,
    profile: String,
}

impl Drop for ExecProcessLease {
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(count) = active.get_mut(&self.profile) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            active.remove(&self.profile);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_worker(
    profile: &ExecProfile,
    inner: &ExecWorkerInner,
    receiver: &Receiver<WorkerCommand>,
    lease: ExecProcessLease,
) {
    if profile.network() == ExecNetworkPolicy::Disabled && !isolate_network_namespace() {
        update_state(inner, |status| {
            status.phase = ExecWorkerPhase::Failed;
            status.failure = Some(ExecWorkerFailure::NetworkIsolationUnavailable);
        });
        drop(lease);
        return;
    }
    let mut respawns = 0;
    loop {
        if inner.stop_requested.load(Ordering::Acquire) {
            break;
        }
        let started_at = Instant::now();
        let mut child = match ChildSession::spawn(profile, &inner.pid) {
            Ok(child) => {
                update_state(inner, |status| {
                    status.phase = ExecWorkerPhase::Running;
                    status.pid = Some(child.child.id());
                    status.failure = None;
                });
                child
            }
            Err(failure) => {
                update_state(inner, |status| {
                    status.phase = ExecWorkerPhase::Failed;
                    status.failure = Some(failure);
                    status.pid = None;
                });
                if !should_respawn(profile, &mut respawns, inner, receiver) {
                    break;
                }
                continue;
            }
        };

        let mut exit_status = None;
        let mut failure = None;
        loop {
            if inner.stop_requested.load(Ordering::Acquire) {
                child.kill();
                break;
            }
            if child.output_overflowed() {
                failure = Some(ExecWorkerFailure::OutputLimit);
                child.kill();
                break;
            }
            if started_at.elapsed() >= profile.limits().timeout {
                failure = Some(ExecWorkerFailure::Timeout);
                child.kill();
                break;
            }
            match receiver.recv_timeout(WORKER_POLL_INTERVAL) {
                Ok(WorkerCommand::Media(frame)) => {
                    inner.queued_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                    if !child.try_send(frame) {
                        update_state(inner, |status| {
                            status.dropped_events = status.dropped_events.saturating_add(1);
                        });
                    }
                }
                Ok(WorkerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    inner.stop_requested.store(true, Ordering::Release);
                    child.kill();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            child.observe(inner);
            if let Some(status) = child.try_wait() {
                exit_status = Some(status);
                break;
            }
        }
        let summary = child.finish();
        inner.pid.store(0, Ordering::Release);
        update_state(inner, |status| {
            status.pid = None;
            status.exit_code = exit_status
                .or(summary.status)
                .and_then(|status| status.code());
            status.stdin_bytes = summary.stdin_bytes;
            status.stdout_bytes = summary.stdout_bytes;
            status.stderr_bytes = summary.stderr_bytes;
            status.output_truncated = summary.output_truncated;
            if failure.is_some() {
                status.failure = failure;
            } else if status.exit_code != Some(0) {
                status.failure = Some(ExecWorkerFailure::ProcessExited);
            }
        });

        if inner.stop_requested.load(Ordering::Acquire) {
            break;
        }
        if !should_respawn(profile, &mut respawns, inner, receiver) {
            let state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let failed = failure.is_some() || state.failure.is_some() || state.exit_code != Some(0);
            drop(state);
            update_state(inner, |status| {
                status.phase = if failed {
                    ExecWorkerPhase::Failed
                } else {
                    ExecWorkerPhase::Exited
                };
            });
            break;
        }
    }
    inner.pid.store(0, Ordering::Release);
    update_state(inner, |status| {
        if status.phase == ExecWorkerPhase::Stopping {
            status.phase = ExecWorkerPhase::Exited;
        }
        status.pid = None;
    });
    drop(lease);
}

fn should_respawn(
    profile: &ExecProfile,
    respawns: &mut usize,
    inner: &ExecWorkerInner,
    receiver: &Receiver<WorkerCommand>,
) -> bool {
    if !profile.respawn() || *respawns >= profile.limits().max_respawns {
        if profile.respawn() && *respawns >= profile.limits().max_respawns {
            update_state(inner, |status| {
                status.failure = Some(ExecWorkerFailure::RespawnLimit);
                status.phase = ExecWorkerPhase::Failed;
            });
        }
        return false;
    }
    *respawns += 1;
    update_state(inner, |status| {
        status.respawns = *respawns;
        status.phase = ExecWorkerPhase::Starting;
    });
    let deadline = Instant::now() + profile.limits().respawn_delay;
    while Instant::now() < deadline {
        if inner.stop_requested.load(Ordering::Acquire) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining.min(WORKER_POLL_INTERVAL)) {
            Ok(WorkerCommand::Media(frame)) => {
                inner.queued_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                update_state(inner, |status| {
                    status.dropped_events = status.dropped_events.saturating_add(1);
                });
            }
            Ok(WorkerCommand::Stop) => {
                inner.stop_requested.store(true, Ordering::Release);
                return false;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
    true
}

struct ChildSession {
    child: Child,
    writer: Option<SyncSender<Vec<u8>>>,
    writer_join: Option<JoinHandle<()>>,
    output_joins: Vec<JoinHandle<OutputSummary>>,
    output_overflow: Arc<AtomicBool>,
    stdin_bytes: Arc<AtomicUsize>,
    stdout_bytes: Arc<AtomicUsize>,
    stderr_bytes: Arc<AtomicUsize>,
    status: Option<ExitStatus>,
}

impl ChildSession {
    fn spawn(profile: &ExecProfile, pid: &AtomicU32) -> Result<Self, ExecWorkerFailure> {
        let mut command = Command::new(profile.executable());
        command
            .args(profile.arguments())
            .current_dir(profile.working_directory())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for entry in profile.environment() {
            command.env(entry.name(), entry.value());
        }
        configure_child(&mut command, profile.network());
        let mut child = command
            .spawn()
            .map_err(|_| ExecWorkerFailure::SpawnFailed)?;
        let child_pid = child.id();
        pid.store(child_pid, Ordering::Release);
        let Some(stdin) = child.stdin.take() else {
            kill_child(&mut child);
            return Err(ExecWorkerFailure::SpawnFailed);
        };
        let Some(stdout) = child.stdout.take() else {
            kill_child(&mut child);
            return Err(ExecWorkerFailure::SpawnFailed);
        };
        let Some(stderr) = child.stderr.take() else {
            kill_child(&mut child);
            return Err(ExecWorkerFailure::SpawnFailed);
        };
        let limits = profile.limits();
        let stdin_bytes = Arc::new(AtomicUsize::new(0));
        let (writer, writer_join) = spawn_stdin_writer(stdin, 1, Arc::clone(&stdin_bytes));
        let output_overflow = Arc::new(AtomicBool::new(false));
        let stdout_bytes = Arc::new(AtomicUsize::new(0));
        let stderr_bytes = Arc::new(AtomicUsize::new(0));
        let output_joins = vec![
            spawn_output_reader(
                stdout,
                limits.max_stdout_bytes,
                Arc::clone(&output_overflow),
                Arc::clone(&stdout_bytes),
            ),
            spawn_output_reader(
                stderr,
                limits.max_stderr_bytes,
                Arc::clone(&output_overflow),
                Arc::clone(&stderr_bytes),
            ),
        ];
        Ok(Self {
            child,
            writer: Some(writer),
            writer_join: Some(writer_join),
            output_joins,
            output_overflow,
            stdin_bytes,
            stdout_bytes,
            stderr_bytes,
            status: None,
        })
    }

    fn try_send(&self, frame: Vec<u8>) -> bool {
        self.writer
            .as_ref()
            .is_some_and(|writer| writer.try_send(frame).is_ok())
    }

    fn observe(&self, inner: &ExecWorkerInner) {
        let stdin_bytes = self.stdin_bytes.load(Ordering::Acquire);
        let stdout_bytes = self.stdout_bytes.load(Ordering::Acquire);
        let stderr_bytes = self.stderr_bytes.load(Ordering::Acquire);
        let output_truncated = self.output_overflowed();
        update_state(inner, |status| {
            status.stdin_bytes = stdin_bytes;
            status.stdout_bytes = stdout_bytes;
            status.stderr_bytes = stderr_bytes;
            status.output_truncated = output_truncated;
        });
    }

    fn try_wait(&mut self) -> Option<ExitStatus> {
        match self.child.try_wait() {
            Ok(status) => {
                if status.is_some() {
                    self.status = status;
                }
                status
            }
            Err(_) => None,
        }
    }

    fn output_overflowed(&self) -> bool {
        self.output_overflow.load(Ordering::Acquire)
    }

    fn kill(&mut self) {
        kill_child(&mut self.child);
    }

    fn finish(mut self) -> OutputSummary {
        if self.status.is_none() {
            self.status = self.child.wait().ok();
        }
        drop(self.writer.take());
        if let Some(join) = self.writer_join.take() {
            let _ = join.join();
        }
        for join in self.output_joins {
            let _ = join.join();
        }
        OutputSummary {
            status: self.status,
            stdin_bytes: self.stdin_bytes.load(Ordering::Acquire),
            stdout_bytes: self.stdout_bytes.load(Ordering::Acquire),
            stderr_bytes: self.stderr_bytes.load(Ordering::Acquire),
            output_truncated: self.output_overflow.load(Ordering::Acquire),
        }
    }
}

struct OutputSummary {
    status: Option<ExitStatus>,
    stdin_bytes: usize,
    stdout_bytes: usize,
    stderr_bytes: usize,
    output_truncated: bool,
}

fn spawn_stdin_writer(
    mut stdin: ChildStdin,
    capacity: usize,
    total: Arc<AtomicUsize>,
) -> (SyncSender<Vec<u8>>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(capacity);
    let join = thread::Builder::new()
        .name("oxiroute-rtmp-exec-stdin".into())
        .spawn(move || {
            while let Ok(frame) = receiver.recv() {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
                total.fetch_add(frame.len(), Ordering::AcqRel);
                if stdin.flush().is_err() {
                    break;
                }
            }
        })
        .expect("exec worker stdin thread must be spawnable");
    (sender, join)
}

fn spawn_output_reader<R: Read + Send + 'static>(
    mut reader: R,
    maximum: usize,
    overflow: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
) -> JoinHandle<OutputSummary> {
    thread::Builder::new()
        .name("oxiroute-rtmp-exec-output".into())
        .spawn(move || {
            let mut buffer = [0; OUTPUT_READ_BUFFER_BYTES];
            let mut bytes = 0_usize;
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        bytes = bytes.saturating_add(read);
                        total.store(bytes.min(maximum), Ordering::Release);
                        if bytes > maximum {
                            overflow.store(true, Ordering::Release);
                        }
                    }
                }
            }
            OutputSummary {
                status: None,
                stdin_bytes: 0,
                stdout_bytes: bytes.min(maximum),
                stderr_bytes: 0,
                output_truncated: bytes > maximum,
            }
        })
        .expect("exec worker output thread must be spawnable")
}

fn update_state(inner: &ExecWorkerInner, update: impl FnOnce(&mut ExecWorkerStatus)) {
    update(
        &mut inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

fn encode_media_event(event: &MediaEvent) -> Vec<u8> {
    let mut frame = Vec::with_capacity(event.payload_len().saturating_add(9));
    frame.push(media_event_kind(event.kind()));
    frame.extend_from_slice(&event.timestamp_ms().to_be_bytes());
    frame
        .extend_from_slice(&(u32::try_from(event.payload_len()).unwrap_or(u32::MAX)).to_be_bytes());
    frame.extend_from_slice(event.payload());
    frame
}

const fn media_event_kind(kind: MediaEventKind) -> u8 {
    match kind {
        MediaEventKind::Metadata => 0,
        MediaEventKind::AacSequenceHeader => 1,
        MediaEventKind::AvcSequenceHeader => 2,
        MediaEventKind::HevcSequenceHeader => 3,
        MediaEventKind::Av1SequenceHeader => 4,
        MediaEventKind::Audio => 5,
        MediaEventKind::VideoKeyframe => 6,
        MediaEventKind::VideoInterframe => 7,
        MediaEventKind::VideoDisposable => 8,
    }
}

fn isolate_network_namespace() -> bool {
    #[cfg(target_os = "linux")]
    {
        #[allow(deprecated)]
        return rustix::thread::unshare(rustix::thread::UnshareFlags::NEWNET).is_ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(unix)]
fn configure_child(command: &mut Command, _network: ExecNetworkPolicy) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_child(_command: &mut Command, _network: ExecNetworkPolicy) {}

fn kill_current_process(pid: &AtomicU32) {
    let raw = pid.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(raw).unwrap_or(i32::MAX)) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

fn kill_child(child: &mut Child) {
    let raw = child.id();
    if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(raw).unwrap_or(i32::MAX)) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, thread, time::Duration};

    use crate::ExecLimits;

    use super::*;

    fn limits(max_queue_messages: usize, max_queue_bytes: usize) -> ExecLimits {
        ExecLimits::new(
            max_queue_messages,
            max_queue_bytes,
            64 * 1024,
            64 * 1024,
            Duration::from_secs(5),
            Duration::from_secs(1),
            1,
            Duration::from_millis(10),
            1,
        )
        .expect("test limits are valid")
    }

    fn profile(
        mode: ExecMode,
        network: ExecNetworkPolicy,
        max_queue_messages: usize,
        max_queue_bytes: usize,
    ) -> ExecProfile {
        ExecProfile::new(
            "capture",
            "live",
            mode,
            ExecTrigger::Publisher,
            PathBuf::from("/bin/cat"),
            Vec::<String>::new(),
            Vec::<crate::ExecEnvironment>::new(),
            PathBuf::from("/tmp"),
            crate::ExecFilesystemPolicy::WorkingDirectory,
            network,
            limits(max_queue_messages, max_queue_bytes),
            false,
        )
        .expect("test profile is valid")
    }

    fn respawning_profile() -> ExecProfile {
        ExecProfile::new(
            "respawn",
            "live",
            ExecMode::Command,
            ExecTrigger::Publisher,
            PathBuf::from("/bin/false"),
            Vec::<String>::new(),
            Vec::<crate::ExecEnvironment>::new(),
            PathBuf::from("/tmp"),
            crate::ExecFilesystemPolicy::WorkingDirectory,
            ExecNetworkPolicy::Inherited,
            limits(1, 64 * 1024),
            true,
        )
        .expect("respawning test profile is valid")
    }

    fn wait_for(
        worker: &ExecWorker,
        predicate: impl Fn(&ExecWorkerStatus) -> bool,
    ) -> ExecWorkerStatus {
        for _ in 0..200 {
            let status = worker.status();
            if predicate(&status) {
                return status;
            }
            thread::sleep(Duration::from_millis(5));
        }
        worker.status()
    }

    fn key() -> StreamKey {
        StreamKey::new("service", "live", "stream")
    }

    #[test]
    fn inherited_transcode_worker_receives_bounded_media_frames() {
        let profiles = ExecProfileSet::new([profile(
            ExecMode::Transcode,
            ExecNetworkPolicy::Inherited,
            8,
            64 * 1024,
        )])
        .expect("profile set");
        let mut workers = profiles.start_publisher("service", &key(), SessionId::new());
        assert_eq!(workers.len(), 1);
        let status = wait_for(&workers[0], |status| {
            status.phase == ExecWorkerPhase::Running
        });
        assert_eq!(status.phase, ExecWorkerPhase::Running);

        let event =
            MediaEvent::audio(10, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
        assert_eq!(workers[0].try_enqueue(&event), ExecEnqueueResult::Queued);
        let status = wait_for(&workers[0], |status| {
            status.stdin_bytes > 0 && status.stdout_bytes > 0
        });
        assert!(status.stdin_bytes >= event.payload_len() + 9);
        assert!(status.stdout_bytes >= event.payload_len() + 9);
        workers.clear();
    }

    #[test]
    fn queue_bytes_fail_closed_without_waiting_for_the_child() {
        let profiles = ExecProfileSet::new([profile(
            ExecMode::Transcode,
            ExecNetworkPolicy::Inherited,
            1,
            1,
        )])
        .expect("profile set");
        let workers = profiles.start_publisher("service", &key(), SessionId::new());
        assert_eq!(workers.len(), 1);
        let event =
            MediaEvent::audio(10, Arc::<[u8]>::from(&b"\xaf\x01\x11"[..])).expect("audio event");
        assert_eq!(workers[0].try_enqueue(&event), ExecEnqueueResult::Dropped);
    }

    #[test]
    fn profile_process_admission_is_bounded() {
        let profiles = ExecProfileSet::new([profile(
            ExecMode::Command,
            ExecNetworkPolicy::Inherited,
            1,
            64 * 1024,
        )])
        .expect("profile set");
        let first = profiles.start_publisher("service", &key(), SessionId::new());
        assert_eq!(first.len(), 1);
        let second = profiles.start_publisher("service", &key(), SessionId::new());
        assert!(second.is_empty());
    }

    #[test]
    fn disabled_network_workers_fail_closed() {
        let profiles = ExecProfileSet::new([profile(
            ExecMode::Command,
            ExecNetworkPolicy::Disabled,
            1,
            64 * 1024,
        )])
        .expect("profile set");
        let workers = profiles.start_publisher("service", &key(), SessionId::new());
        let status = wait_for(&workers[0], |status| {
            matches!(
                status.phase,
                ExecWorkerPhase::Failed | ExecWorkerPhase::Running
            )
        });
        if status.phase == ExecWorkerPhase::Failed {
            assert_eq!(
                status.failure,
                Some(ExecWorkerFailure::NetworkIsolationUnavailable)
            );
        }
    }

    #[test]
    fn respawn_count_is_bounded_and_reported() {
        let profiles = ExecProfileSet::new([respawning_profile()]).expect("profile set");
        let workers = profiles.start_publisher("service", &key(), SessionId::new());
        let status = wait_for(&workers[0], |status| {
            status.phase == ExecWorkerPhase::Failed
        });
        assert_eq!(status.respawns, 1);
        assert_eq!(status.failure, Some(ExecWorkerFailure::RespawnLimit));
    }
}
