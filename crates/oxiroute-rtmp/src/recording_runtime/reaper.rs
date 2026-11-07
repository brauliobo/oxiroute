#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub(crate) struct RecorderReaperHandle {
    pub(super) queue: Arc<ReaperQueue>,
    pub(super) registry: Weak<RtmpRegistry>,
    cleanup: RecorderCleanupHandle,
}

#[derive(Clone)]
pub(crate) struct RecorderShutdownControl {
    queue: Arc<ReaperQueue>,
    cleanup: RecorderCleanupHandle,
}

pub(crate) struct RecorderReaper {
    queue: Arc<ReaperQueue>,
    thread: Option<JoinHandle<()>>,
    cleanup: RecorderCleanupHandle,
}

pub(crate) struct RecorderReaperOwner {
    reaper: Mutex<Option<RecorderReaper>>,
    shutdown: RtmpRecorderShutdown,
}

type RecorderRetirement = Box<dyn FnOnce() + Send + 'static>;

/// Completion handle for one RTMP recorder/reaper shutdown lifecycle.
#[derive(Clone)]
pub struct RtmpRecorderShutdown {
    queue: Arc<ReaperQueue>,
    retirement: Arc<Mutex<RecorderRetirementState>>,
}

#[derive(Clone)]
struct RecorderCleanupHandle {
    sender: Sender<CleanupCommand>,
}

pub(super) enum CleanupCommand {
    Task {
        task: ReapTask,
        queue: Option<Arc<ReaperQueue>>,
    },
    Reaper {
        thread: JoinHandle<()>,
        queue: Arc<ReaperQueue>,
    },
    Wake,
}

pub(super) struct ReapCompletion {
    pub(super) registry: Weak<RtmpRegistry>,
    pub(super) context: RecorderCommandContext,
}

pub(super) struct ReaperQueue {
    pub(super) sender: Sender<ReaperCommand>,
    pub(super) capacity: usize,
    pub(super) retirement: Arc<Mutex<RecorderRetirementState>>,
    pub(super) state: Mutex<ReaperQueueState>,
    pub(super) available: Condvar,
}

pub(super) struct RecorderRetirementState {
    pub(super) callback: Option<RecorderRetirement>,
    pub(super) ran: bool,
}

pub(super) struct ReaperQueueState {
    pub(super) accepting: bool,
    pub(super) failed: bool,
    pub(super) pending: usize,
    pub(super) shutdown_deadline: Option<Instant>,
    pub(super) reaper_finished: bool,
}

pub(super) enum ReaperCommand {
    Reap(ReapTask),
    Shutdown {
        deadline: Option<Instant>,
    },
    #[cfg(test)]
    Panic,
}

pub(super) struct ReapTask {
    pub(super) supervisor: Option<RecorderWorkerSupervisor>,
    pub(super) worker_generation: u64,
    pub(super) controller: Weak<RecorderController>,
    pub(super) completion: Option<ReapCompletion>,
    pub(super) deadline: Instant,
    pub(super) completion_at_unix_ms: u64,
    pub(super) cancelled: bool,
}

pub(super) enum ReaperSubmitError {
    Closed(Box<ReapTask>),
    Failed(Box<ReapTask>),
}

impl RecorderReaper {
    pub(crate) fn start(
        capacity: usize,
        registry: Weak<RtmpRegistry>,
    ) -> (Arc<RecorderReaperOwner>, RecorderReaperHandle) {
        let cleanup = RecorderCleanupHandle::start();
        let (sender, receiver) = mpsc::channel();
        let queue = Arc::new(ReaperQueue {
            sender,
            capacity: capacity.max(1),
            retirement: Arc::new(Mutex::new(RecorderRetirementState {
                callback: None,
                ran: false,
            })),
            state: Mutex::new(ReaperQueueState {
                accepting: true,
                failed: false,
                pending: 0,
                shutdown_deadline: None,
                reaper_finished: false,
            }),
            available: Condvar::new(),
        });
        let worker_queue = Arc::clone(&queue);
        let worker_cleanup = cleanup.clone();
        let thread = thread::Builder::new()
            .name("rtmp-recorder-reaper".to_owned())
            .spawn(move || supervise_reaper(&receiver, &worker_queue, &worker_cleanup))
            .expect("recorder reaper thread must start");
        (
            Arc::new(RecorderReaperOwner {
                reaper: Mutex::new(Some(Self {
                    queue: Arc::clone(&queue),
                    thread: Some(thread),
                    cleanup: cleanup.clone(),
                })),
                shutdown: RtmpRecorderShutdown {
                    queue: Arc::clone(&queue),
                    retirement: Arc::clone(&queue.retirement),
                },
            }),
            RecorderReaperHandle {
                queue,
                registry,
                cleanup,
            },
        )
    }

    fn initiate_shutdown(mut self, deadline: Option<Instant>) {
        self.queue.shutdown(deadline);
        if let Some(thread) = self.thread.take() {
            self.cleanup.submit_reaper(thread, &self.queue);
        }
    }
}

impl RecorderReaperOwner {
    pub(crate) fn shutdown_handle(&self) -> RtmpRecorderShutdown {
        self.shutdown.clone()
    }

    pub(crate) fn initiate_shutdown(&self, deadline: Instant) -> RtmpRecorderShutdown {
        let reaper = self
            .reaper
            .lock()
            .expect("recorder reaper owner mutex poisoned")
            .take();
        if let Some(reaper) = reaper {
            reaper.initiate_shutdown(Some(deadline));
        } else {
            self.shutdown.queue.shutdown(Some(deadline));
        }
        self.shutdown.clone()
    }
}

impl RtmpRecorderShutdown {
    /// Returns true when both handles observe the same recorder shutdown lifecycle.
    #[must_use]
    pub fn is_same_lifecycle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue)
    }

    /// Returns true once the reaper has exited and every tracked worker has been reconciled.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.queue.run_retirement_if_complete();
        self.queue.shutdown_complete()
    }

    /// Waits until completion or the supplied absolute deadline.
    #[must_use]
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let complete = self.queue.wait_until_shutdown_complete(deadline);
        self.queue.run_retirement_if_complete();
        complete
    }

    pub(crate) fn set_retirement(&self, retire: impl FnOnce() + Send + 'static) {
        let mut retirement = self
            .retirement
            .lock()
            .expect("recorder retirement mutex poisoned");
        if !retirement.ran && retirement.callback.is_none() {
            retirement.callback = Some(Box::new(retire));
        }
        drop(retirement);
        self.queue.run_retirement_if_complete();
    }
}

impl Drop for RecorderReaperOwner {
    fn drop(&mut self) {
        if let Some(reaper) = self
            .reaper
            .get_mut()
            .expect("recorder reaper owner mutex poisoned")
            .take()
        {
            reaper.initiate_shutdown(None);
        }
    }
}

impl RecorderReaperHandle {
    pub(crate) fn shutdown_control(&self) -> RecorderShutdownControl {
        RecorderShutdownControl {
            queue: Arc::clone(&self.queue),
            cleanup: self.cleanup.clone(),
        }
    }

    pub(super) fn submit(
        &self,
        worker: RecorderWorker,
        worker_generation: u64,
        controller: Weak<RecorderController>,
        completion: Option<ReapCompletion>,
        completion_at_unix_ms: u64,
    ) {
        let supervisor = worker.into_supervisor();
        let deadline = Instant::now() + supervisor.shutdown_timeout();
        let task = ReapTask {
            supervisor: Some(supervisor),
            worker_generation,
            controller,
            completion,
            deadline,
            completion_at_unix_ms,
            cancelled: false,
        };
        match self.queue.submit(task) {
            Ok(()) => {}
            Err(ReaperSubmitError::Closed(task) | ReaperSubmitError::Failed(task)) => {
                self.cleanup
                    .submit_task(*task, Some(Arc::clone(&self.queue)));
            }
        }
    }

    pub(super) fn try_submit(
        &self,
        worker: RecorderWorker,
        worker_generation: u64,
        controller: Weak<RecorderController>,
        completion_at_unix_ms: u64,
    ) -> Result<(), RecorderWorker> {
        let mut state = self
            .queue
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        if !state.accepting || state.failed || state.pending >= self.queue.capacity {
            return Err(worker);
        }
        let supervisor = worker.into_supervisor();
        let task = ReapTask {
            deadline: Instant::now() + supervisor.shutdown_timeout(),
            supervisor: Some(supervisor),
            worker_generation,
            controller,
            completion: None,
            completion_at_unix_ms,
            cancelled: false,
        };
        state.pending += 1;
        if let Err(error) = self.queue.sender.send(ReaperCommand::Reap(task)) {
            state.accepting = false;
            state.failed = true;
            state.shutdown_deadline = Some(Instant::now());
            drop(state);
            self.queue.available.notify_all();
            let ReaperCommand::Reap(mut task) = error.0 else {
                unreachable!("reserved submission only sends reap commands");
            };
            task.deadline = Instant::now();
            self.cleanup
                .submit_task(task, Some(Arc::clone(&self.queue)));
        }
        Ok(())
    }
}

impl RecorderShutdownControl {
    pub(crate) fn initiate_shutdown(&self, deadline: Instant) -> RtmpRecorderShutdown {
        self.queue.shutdown(Some(deadline));
        self.cleanup.wake();
        RtmpRecorderShutdown {
            queue: Arc::clone(&self.queue),
            retirement: Arc::clone(&self.queue.retirement),
        }
    }
}

impl RecorderCleanupHandle {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("rtmp-recorder-cleanup".to_owned())
            .spawn(move || run_cleanup(&receiver))
            .expect("recorder cleanup lifecycle must start");
        drop(thread);
        Self { sender }
    }

    fn submit_task(&self, task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
        if let Err(error) = self.sender.send(CleanupCommand::Task { task, queue }) {
            let CleanupCommand::Task { task, queue } = error.0 else {
                unreachable!("task submission only sends cleanup tasks");
            };
            abandon_cleanup_task(task, queue);
        }
    }

    fn submit_reaper(&self, thread: JoinHandle<()>, queue: &Arc<ReaperQueue>) {
        if let Err(error) = self.sender.send(CleanupCommand::Reaper {
            thread,
            queue: Arc::clone(queue),
        }) {
            let CleanupCommand::Reaper { thread, .. } = error.0 else {
                unreachable!("reaper submission only sends reaper handles");
            };
            drop(thread);
        }
    }

    fn wake(&self) {
        let _ = self.sender.send(CleanupCommand::Wake);
    }
}

fn supervise_reaper(
    receiver: &Receiver<ReaperCommand>,
    queue: &Arc<ReaperQueue>,
    cleanup: &RecorderCleanupHandle,
) {
    let mut tasks = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_reaper(receiver, queue, &mut tasks);
    }));
    if result.is_ok() {
        return;
    }
    queue.fail();
    for task in tasks.drain(..) {
        let mut task = task;
        task.deadline = Instant::now();
        cleanup.submit_task(task, Some(Arc::clone(queue)));
    }
    while let Ok(command) = receiver.try_recv() {
        if let ReaperCommand::Reap(task) = command {
            let mut task = task;
            task.deadline = Instant::now();
            cleanup.submit_task(task, Some(Arc::clone(queue)));
        }
    }
}

fn run_reaper(
    receiver: &Receiver<ReaperCommand>,
    queue: &Arc<ReaperQueue>,
    tasks: &mut Vec<ReapTask>,
) {
    let mut shutting_down = false;
    let mut shutdown_deadline: Option<Instant> = None;
    loop {
        if shutting_down && tasks.is_empty() {
            return;
        }
        let command = if tasks.is_empty() {
            if let Ok(command) = receiver.recv() {
                Some(command)
            } else {
                shutting_down = true;
                None
            }
        } else {
            match receiver.recv_timeout(REAPER_POLL_INTERVAL) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    shutting_down = true;
                    None
                }
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            }
        };
        match command {
            Some(ReaperCommand::Reap(task)) => tasks.push(task),
            Some(ReaperCommand::Shutdown { deadline }) => {
                shutting_down = true;
                shutdown_deadline = earliest_deadline(shutdown_deadline, deadline);
            }
            #[cfg(test)]
            Some(ReaperCommand::Panic) => panic!("injected recorder reaper panic"),
            None => {}
        }
        loop {
            match receiver.try_recv() {
                Ok(ReaperCommand::Reap(task)) => tasks.push(task),
                Ok(ReaperCommand::Shutdown { deadline }) => {
                    shutting_down = true;
                    shutdown_deadline = earliest_deadline(shutdown_deadline, deadline);
                    break;
                }
                #[cfg(test)]
                Ok(ReaperCommand::Panic) => panic!("injected recorder reaper panic"),
                Err(TryRecvError::Disconnected) => {
                    shutting_down = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let now = Instant::now();
        for task in &mut *tasks {
            if now >= task.deadline || shutdown_deadline.is_some_and(|deadline| now >= deadline) {
                task.cancel();
            }
        }
        let mut index = 0;
        while index < tasks.len() {
            let finished = tasks[index]
                .supervisor
                .as_ref()
                .is_some_and(RecorderWorkerSupervisor::is_finished);
            if !finished {
                index += 1;
                continue;
            }
            finish_cleanup_task(tasks.swap_remove(index), Some(Arc::clone(queue)));
        }
        if shutting_down && tasks.is_empty() {
            return;
        }
    }
}

pub(super) fn earliest_deadline(
    current: Option<Instant>,
    next: Option<Instant>,
) -> Option<Instant> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.min(next)),
        (Some(current), None) => Some(current),
        (None, next) => next,
    }
}
