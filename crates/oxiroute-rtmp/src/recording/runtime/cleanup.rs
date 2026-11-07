#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn run_cleanup(receiver: &Receiver<CleanupCommand>) {
    let mut tasks = Vec::new();
    let mut reapers = Vec::new();
    let mut disconnected = false;
    loop {
        let command = if tasks.is_empty() && reapers.is_empty() {
            if let Ok(command) = receiver.recv() {
                Some(command)
            } else {
                disconnected = true;
                None
            }
        } else {
            match receiver.recv_timeout(REAPER_POLL_INTERVAL) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    None
                }
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            }
        };
        if let Some(command) = command {
            retain_cleanup_command(command, &mut tasks, &mut reapers);
        }
        while let Ok(command) = receiver.try_recv() {
            retain_cleanup_command(command, &mut tasks, &mut reapers);
        }

        let mut index = 0;
        while index < tasks.len() {
            let finished = tasks[index]
                .0
                .supervisor
                .as_ref()
                .is_some_and(RecorderWorkerSupervisor::is_finished);
            if !finished {
                let process_deadline = tasks[index]
                    .1
                    .as_ref()
                    .and_then(|queue| queue.shutdown_deadline());
                let deadline = earliest_deadline(Some(tasks[index].0.deadline), process_deadline)
                    .expect("a cleanup task always has a deadline");
                if Instant::now() >= deadline {
                    tasks[index].0.cancel();
                }
                index += 1;
                continue;
            }
            let (task, queue) = tasks.swap_remove(index);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                finish_cleanup_task(task, queue);
            }));
        }

        let mut index = 0;
        while index < reapers.len() {
            if !reapers[index].0.is_finished() {
                index += 1;
                continue;
            }
            let (thread, queue) = reapers.swap_remove(index);
            let _ = thread.join();
            queue.finish_reaper();
        }

        if disconnected && tasks.is_empty() && reapers.is_empty() {
            return;
        }
    }
}

fn retain_cleanup_command(
    command: CleanupCommand,
    tasks: &mut Vec<(ReapTask, Option<Arc<ReaperQueue>>)>,
    reapers: &mut Vec<(JoinHandle<()>, Arc<ReaperQueue>)>,
) {
    match command {
        CleanupCommand::Task { task, queue } => tasks.push((task, queue)),
        CleanupCommand::Reaper { thread, queue } => reapers.push((thread, queue)),
        CleanupCommand::Wake => {}
    }
}

pub(super) fn finish_cleanup_task(task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
    let _completion = QueueCompletion(queue);
    task.finish();
}

pub(super) fn abandon_cleanup_task(mut task: ReapTask, queue: Option<Arc<ReaperQueue>>) {
    let _completion = QueueCompletion(queue);
    task.cancel();
    task.abandon();
}

struct QueueCompletion(Option<Arc<ReaperQueue>>);

impl Drop for QueueCompletion {
    fn drop(&mut self) {
        if let Some(queue) = self.0.take() {
            queue.complete();
        }
    }
}

impl ReaperQueue {
    pub(super) fn admission(&self) -> Option<MutexGuard<'_, ReaperQueueState>> {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting.then_some(state)
    }

    pub(super) fn submit(&self, mut task: ReapTask) -> Result<(), ReaperSubmitError> {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state = self
            .available
            .wait_while(state, |state| {
                state.accepting && state.pending >= self.capacity
            })
            .expect("recorder reaper queue mutex poisoned");
        if state.failed {
            task.deadline = Instant::now();
            state.pending += 1;
            return Err(ReaperSubmitError::Failed(Box::new(task)));
        }
        if !state.accepting {
            task.deadline = earliest_deadline(Some(task.deadline), state.shutdown_deadline)
                .expect("a reaper task always has a deadline");
            state.pending += 1;
            return Err(ReaperSubmitError::Closed(Box::new(task)));
        }
        state.pending += 1;
        match self.sender.send(ReaperCommand::Reap(task)) {
            Ok(()) => Ok(()),
            Err(error) => {
                state.pending -= 1;
                drop(state);
                self.available.notify_all();
                let ReaperCommand::Reap(mut task) = error.0 else {
                    unreachable!("submit only sends reap commands");
                };
                task.deadline = Instant::now();
                state = self
                    .state
                    .lock()
                    .expect("recorder reaper queue mutex poisoned");
                state.accepting = false;
                state.failed = true;
                drop(state);
                self.available.notify_all();
                Err(ReaperSubmitError::Failed(Box::new(task)))
            }
        }
    }

    pub(super) fn shutdown(&self, deadline: Option<Instant>) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting = false;
        state.shutdown_deadline = earliest_deadline(state.shutdown_deadline, deadline);
        let effective_deadline = state.shutdown_deadline;
        let _ = self.sender.send(ReaperCommand::Shutdown {
            deadline: effective_deadline,
        });
        drop(state);
        self.available.notify_all();
        self.run_retirement_if_complete();
    }

    pub(super) fn shutdown_deadline(&self) -> Option<Instant> {
        self.state
            .lock()
            .expect("recorder reaper queue mutex poisoned")
            .shutdown_deadline
    }

    fn complete(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.pending = state
            .pending
            .checked_sub(1)
            .expect("recorder reaper completed an untracked task");
        drop(state);
        self.available.notify_one();
        self.run_retirement_if_complete();
    }

    pub(super) fn fail(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.accepting = false;
        state.failed = true;
        state.shutdown_deadline = Some(Instant::now());
        drop(state);
        self.available.notify_all();
    }

    fn finish_reaper(&self) {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        state.reaper_finished = true;
        drop(state);
        self.available.notify_all();
        self.run_retirement_if_complete();
    }

    pub(super) fn run_retirement_if_complete(&self) {
        if !self.shutdown_complete() {
            return;
        }
        let retirement = {
            let mut state = self
                .retirement
                .lock()
                .expect("recorder retirement mutex poisoned");
            if state.ran {
                None
            } else if let Some(callback) = state.callback.take() {
                state.ran = true;
                Some(callback)
            } else {
                None
            }
        };
        if let Some(retirement) = retirement {
            retirement();
        }
    }

    pub(super) fn shutdown_complete(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        !state.accepting && state.pending == 0 && state.reaper_finished
    }

    pub(super) fn wait_until_shutdown_complete(&self, deadline: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        loop {
            if !state.accepting && state.pending == 0 && state.reaper_finished {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .available
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("recorder reaper queue mutex poisoned")
                .0;
        }
    }

    #[cfg(test)]
    pub(super) fn panic_reaper(&self) {
        let _ = self.sender.send(ReaperCommand::Panic);
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> (bool, usize) {
        let state = self
            .state
            .lock()
            .expect("recorder reaper queue mutex poisoned");
        (state.accepting, state.pending)
    }
}

impl ReapTask {
    pub(super) fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.supervisor
            .as_ref()
            .expect("unfinished reaper task owns a supervisor")
            .cancel();
        self.cancelled = true;
    }

    fn finish(mut self) {
        let status = self
            .supervisor
            .take()
            .expect("unfinished reaper task owns a supervisor")
            .join();
        if let Some(controller) = self.controller.upgrade() {
            controller.finish_reap(
                self.worker_generation,
                status.clone(),
                self.completion_at_unix_ms,
            );
        }
        if let Some(completion) = self.completion
            && let Some(registry) = completion.registry.upgrade()
        {
            registry.complete_worker_stop(completion.context, &status);
        }
    }

    fn abandon(mut self) {
        let status = self
            .supervisor
            .take()
            .expect("unfinished reaper task owns a supervisor")
            .detach();
        if let Some(controller) = self.controller.upgrade() {
            controller.finish_reap(
                self.worker_generation,
                status.clone(),
                self.completion_at_unix_ms,
            );
        }
        if let Some(completion) = self.completion
            && let Some(registry) = completion.registry.upgrade()
        {
            registry.complete_worker_stop(completion.context, &status);
        }
    }
}
