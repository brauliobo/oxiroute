#[allow(clippy::wildcard_imports)]
use super::*;

type FinalizerJob = Box<dyn FnOnce() + Send + 'static>;

struct FinalizerJobEntry {
    id: u64,
    job: FinalizerJob,
}

pub(super) struct RecordingFinalizer {
    shared: Arc<FinalizerShared>,
    pub(super) threads: Vec<JoinHandle<()>>,
}

struct FinalizerShared {
    state: Mutex<FinalizerState>,
    available: Condvar,
    space_available: Condvar,
    queue_capacity: usize,
}

#[derive(Default)]
struct FinalizerState {
    jobs: VecDeque<FinalizerJobEntry>,
    next_job_id: u64,
    stopping: bool,
}

pub(crate) struct FinalizerTicket {
    shared: Weak<FinalizerShared>,
    id: u64,
}

#[derive(Clone)]
pub(crate) struct RecordingFinalizerAuthority {
    pub(super) incarnation: u64,
    pub(super) shared: Arc<StoreShared>,
}

impl RecordingFinalizerAuthority {
    pub(crate) fn submit(&self, job: FinalizerJob) -> FinalizerTicket {
        let shared = Arc::clone(&self.shared);
        let incarnation = self.incarnation;
        self.shared.finalizer.submit(Box::new(move || {
            // The job still runs after retirement so its file can become recoverable. Publication
            // remains fenced by the same incarnation inside RecordingFile::commit.
            let _retired = !shared.incarnation_active(incarnation);
            job();
        }))
    }
}

impl RecordingFinalizer {
    pub(super) fn new(max_active_recorders: usize) -> Result<Self, RecordingStoreError> {
        let queue_capacity = max_active_recorders
            .saturating_mul(MAX_PENDING_FINALIZATIONS_PER_RECORDER)
            .max(1);
        let thread_count = queue_capacity.min(MAX_FINALIZER_THREADS);
        let shared = Arc::new(FinalizerShared {
            state: Mutex::new(FinalizerState::default()),
            available: Condvar::new(),
            space_available: Condvar::new(),
            queue_capacity,
        });
        let mut threads = Vec::with_capacity(thread_count);
        for index in 0..thread_count {
            let worker_shared = Arc::clone(&shared);
            match thread::Builder::new()
                .name(format!("rtmp-recording-finalizer-{index}"))
                .spawn(move || run_finalizer(&worker_shared))
            {
                Ok(thread) => threads.push(thread),
                Err(source) => {
                    shared.lock().stopping = true;
                    shared.available.notify_all();
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(RecordingStoreError::FinalizerThreadSpawn(source));
                }
            }
        }
        Ok(Self { shared, threads })
    }

    fn submit(&self, job: FinalizerJob) -> FinalizerTicket {
        let mut state = self.shared.lock();
        while state.jobs.len() >= self.shared.queue_capacity && !state.stopping {
            state = self
                .shared
                .space_available
                .wait(state)
                .expect("recording finalizer mutex poisoned while waiting for capacity");
        }
        assert!(
            !state.stopping,
            "recording finalizer stopped before its store"
        );
        let id = state.next_job_id;
        state.next_job_id = state
            .next_job_id
            .checked_add(1)
            .expect("recording finalizer job identity exhausted");
        state.jobs.push_back(FinalizerJobEntry { id, job });
        drop(state);
        self.shared.available.notify_one();
        FinalizerTicket {
            shared: Arc::downgrade(&self.shared),
            id,
        }
    }
}

impl FinalizerTicket {
    pub(crate) fn cancel_queued(&self) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut state = shared.lock();
        let Some(position) = state.jobs.iter().position(|job| job.id == self.id) else {
            return false;
        };
        let job = state
            .jobs
            .remove(position)
            .expect("queued finalization position remains valid");
        drop(state);
        shared.space_available.notify_one();
        drop(job);
        true
    }
}

impl Drop for RecordingFinalizer {
    fn drop(&mut self) {
        self.shared.lock().stopping = true;
        self.shared.available.notify_all();
        self.shared.space_available.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl FinalizerShared {
    fn lock(&self) -> MutexGuard<'_, FinalizerState> {
        self.state
            .lock()
            .expect("recording finalizer mutex poisoned")
    }
}

fn run_finalizer(shared: &FinalizerShared) {
    loop {
        let mut state = shared.lock();
        while state.jobs.is_empty() && !state.stopping {
            state = shared
                .available
                .wait(state)
                .expect("recording finalizer mutex poisoned while waiting");
        }
        let Some(job) = state.jobs.pop_front() else {
            return;
        };
        drop(state);
        shared.space_available.notify_one();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.job));
    }
}
