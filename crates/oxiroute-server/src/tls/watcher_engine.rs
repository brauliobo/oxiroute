use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::certificate::PublicationGate;

#[derive(Clone, Copy)]
pub(super) struct WatcherTiming {
    pub rescan_interval: Duration,
    pub event_debounce: Duration,
    pub event_max_delay: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WatcherStatus {
    pub running: bool,
    pub degraded: bool,
    pub coalesced_events: u64,
    pub ignored_access_events: u64,
    pub backend_errors: u64,
    pub watch_recoveries: u64,
    pub watch_refreshes: u64,
    pub rescans: u64,
    pub periodic_rescans: u64,
    pub reconciliation_failures: u64,
}

#[derive(Default)]
struct WatcherState {
    running: AtomicBool,
    backend_degraded: AtomicBool,
    reconciliation_degraded: AtomicBool,
    coalesced_events: AtomicU64,
    ignored_access_events: AtomicU64,
    backend_errors: AtomicU64,
    watch_recoveries: AtomicU64,
    watch_refreshes: AtomicU64,
    rescans: AtomicU64,
    periodic_rescans: AtomicU64,
    reconciliation_failures: AtomicU64,
}

impl WatcherState {
    fn snapshot(&self) -> WatcherStatus {
        WatcherStatus {
            running: self.running.load(Ordering::Acquire),
            degraded: self.backend_degraded.load(Ordering::Acquire)
                || self.reconciliation_degraded.load(Ordering::Acquire),
            coalesced_events: self.coalesced_events.load(Ordering::Relaxed),
            ignored_access_events: self.ignored_access_events.load(Ordering::Relaxed),
            backend_errors: self.backend_errors.load(Ordering::Relaxed),
            watch_recoveries: self.watch_recoveries.load(Ordering::Relaxed),
            watch_refreshes: self.watch_refreshes.load(Ordering::Relaxed),
            rescans: self.rescans.load(Ordering::Relaxed),
            periodic_rescans: self.periodic_rescans.load(Ordering::Relaxed),
            reconciliation_failures: self.reconciliation_failures.load(Ordering::Relaxed),
        }
    }

    fn mark_backend_degraded(&self) {
        self.backend_degraded.store(true, Ordering::Release);
        self.backend_errors.fetch_add(1, Ordering::Relaxed);
    }

    fn mark_backend_recovered(&self) {
        if self.backend_degraded.swap(false, Ordering::AcqRel) {
            self.watch_recoveries.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub(super) struct WatcherMonitor {
    state: Arc<WatcherState>,
}

impl WatcherMonitor {
    pub(super) fn status(&self) -> WatcherStatus {
        self.state.snapshot()
    }
}

#[derive(Clone)]
pub(super) struct WakeQueue {
    sender: SyncSender<()>,
    state: Arc<WatcherState>,
}

impl WakeQueue {
    pub(super) fn new() -> (Self, Receiver<()>, WatcherMonitor) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(WatcherState::default());
        (
            Self {
                sender,
                state: Arc::clone(&state),
            },
            receiver,
            WatcherMonitor { state },
        )
    }

    pub(super) fn event(&self) {
        match self.sender.try_send(()) {
            Ok(()) | Err(TrySendError::Disconnected(())) => {}
            Err(TrySendError::Full(())) => {
                self.state.coalesced_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn ignored_access(&self) {
        self.state
            .ignored_access_events
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn backend_error(&self) {
        self.state.mark_backend_degraded();
        self.event();
    }

    #[cfg(test)]
    pub(super) fn backend_recovered(&self) {
        self.state.mark_backend_recovered();
    }
}

pub(super) fn handle_notify_event(wake: &WakeQueue, event: notify::Result<notify::Event>) {
    match event {
        Ok(event) if event.need_rescan() => wake.backend_error(),
        Ok(event) if event.kind.is_access() => wake.ignored_access(),
        Ok(_event) => wake.event(),
        Err(_error) => wake.backend_error(),
    }
}

pub(super) enum ReconcileResult {
    Completed { failures: u64 },
    Stopped,
}

pub(super) trait WatcherSource: Send + 'static {
    fn reconcile(&mut self, gate: &PublicationGate) -> ReconcileResult;
    fn refresh(&mut self, wake: &WakeQueue) -> bool;
}

pub(super) enum WatcherStartError<E> {
    Source(E),
    Thread(io::Error),
}

pub(super) struct WatcherEngine {
    gate: Arc<PublicationGate>,
    wake: WakeQueue,
    monitor: WatcherMonitor,
    worker: Option<JoinHandle<()>>,
}

impl WatcherEngine {
    pub(super) fn start<S, E>(
        thread_name: &str,
        timing: WatcherTiming,
        source: impl FnOnce(&WakeQueue) -> Result<S, E>,
    ) -> Result<Self, WatcherStartError<E>>
    where
        S: WatcherSource,
    {
        let (wake, receiver, monitor) = WakeQueue::new();
        let mut source = source(&wake).map_err(WatcherStartError::Source)?;
        let gate = Arc::new(PublicationGate::new());
        let worker_gate = Arc::clone(&gate);
        let worker_state = Arc::clone(&monitor.state);
        let worker_wake = wake.clone();
        monitor.state.running.store(true, Ordering::Release);
        let worker = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let _running = RunningGuard(Arc::clone(&worker_state));
                run_worker(
                    &receiver,
                    timing,
                    &worker_gate,
                    &worker_state,
                    &worker_wake,
                    &mut source,
                );
            })
            .map_err(|error| {
                monitor.state.running.store(false, Ordering::Release);
                WatcherStartError::Thread(error)
            })?;
        wake.event();
        Ok(Self {
            gate,
            wake,
            monitor,
            worker: Some(worker),
        })
    }

    pub(super) fn status(&self) -> WatcherStatus {
        self.monitor.status()
    }

    pub(super) fn monitor(&self) -> WatcherMonitor {
        self.monitor.clone()
    }

    pub(super) fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    pub(super) fn shutdown(&mut self) {
        self.gate.stop();
        self.wake.event();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.monitor.state.running.store(false, Ordering::Release);
    }
}

impl Drop for WatcherEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct RunningGuard(Arc<WatcherState>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

fn run_worker(
    receiver: &Receiver<()>,
    timing: WatcherTiming,
    gate: &PublicationGate,
    state: &WatcherState,
    wake: &WakeQueue,
    source: &mut impl WatcherSource,
) {
    loop {
        let periodic = match receiver.recv_timeout(timing.rescan_interval) {
            Ok(()) => {
                if !debounce_events(receiver, gate, timing) {
                    return;
                }
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if gate.is_stopped() {
            return;
        }
        state.rescans.fetch_add(1, Ordering::Relaxed);
        if periodic {
            state.periodic_rescans.fetch_add(1, Ordering::Relaxed);
        }
        let failures = match source.reconcile(gate) {
            ReconcileResult::Completed { failures } => failures,
            ReconcileResult::Stopped => return,
        };
        state
            .reconciliation_degraded
            .store(failures > 0, Ordering::Release);
        state
            .reconciliation_failures
            .fetch_add(failures, Ordering::Relaxed);
        if source.refresh(wake) {
            state.watch_refreshes.fetch_add(1, Ordering::Relaxed);
            state.mark_backend_recovered();
        } else {
            state.mark_backend_degraded();
        }
    }
}

fn debounce_events(receiver: &Receiver<()>, gate: &PublicationGate, timing: WatcherTiming) -> bool {
    let mut window = DebounceWindow::new(Instant::now());
    loop {
        if gate.is_stopped() {
            return false;
        }
        let now = Instant::now();
        let deadline = window.deadline(timing.event_debounce, timing.event_max_delay);
        if now >= deadline {
            return true;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(()) => window.note_event(Instant::now()),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

pub(super) struct DebounceWindow {
    first_event: Instant,
    last_event: Instant,
}

impl DebounceWindow {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            first_event: now,
            last_event: now,
        }
    }

    pub(super) fn note_event(&mut self, now: Instant) {
        self.last_event = now;
    }

    pub(super) fn deadline(&self, debounce: Duration, max_delay: Duration) -> Instant {
        (self.last_event + debounce).min(self.first_event + max_delay)
    }
}
