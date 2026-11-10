use std::{
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use crate::{Controller, Policy};

/// Time between two looks at the workers.
const TICK: Duration = Duration::from_millis(20);

/// A worker whose connections exceed the average by more than this share (and at least [`MIN_SLACK`]) sheds some.
const SLACK_DIVISOR: usize = 8;
const MIN_SLACK: usize = 2;

/// What the controller reads from the workers of one listener runtime.
pub trait Workers: Send + Sync + 'static {
    fn len(&self) -> usize;
    /// Nanoseconds the worker has spent running tasks since it started; only the difference between two readings
    /// matters.
    fn busy_nanos(&self, worker: usize) -> u64;
    /// Connections placed on the worker and not finished.
    fn connections(&self, worker: usize) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The counters of one runtime's scaling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub workers: usize,
    pub active: usize,
    /// Connections moved between workers since the start.
    pub relocations: u64,
}

struct Shared {
    workers: usize,
    active: AtomicUsize,
    rebalance_above: AtomicUsize,
    relocations: AtomicU64,
    sleeping: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn stats(&self) -> Stats {
        Stats {
            workers: self.workers,
            active: self.active.load(Ordering::Relaxed),
            relocations: self.relocations.load(Ordering::Relaxed),
        }
    }
}

type Registry = Mutex<Vec<(String, Weak<Shared>)>>;

static SCALERS: Registry = Mutex::new(Vec::new());

fn registry() -> std::sync::MutexGuard<'static, Vec<(String, Weak<Shared>)>> {
    SCALERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The statistics of every live runtime that scales its workers, by runtime name.
#[must_use]
pub fn process_stats() -> Vec<(String, Stats)> {
    registry()
        .iter()
        .filter_map(|(name, shared)| Some((name.clone(), shared.upgrade()?.stats())))
        .collect()
}

/// The active set of one listener runtime: placement and relocation read it, the controller thread writes it.
pub struct Scaling {
    name: String,
    policy: Policy,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    waker: Mutex<Option<Thread>>,
}

impl Scaling {
    /// All `workers` active, no controller running yet.
    #[must_use]
    pub fn new(name: &str, policy: Policy, workers: usize) -> Self {
        let shared = Arc::new(Shared {
            workers,
            active: AtomicUsize::new(workers),
            rebalance_above: AtomicUsize::new(usize::MAX),
            relocations: AtomicU64::new(0),
            sleeping: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let mut live = registry();
        live.retain(|(_, shared)| shared.strong_count() > 0);
        live.push((name.to_owned(), Arc::downgrade(&shared)));
        drop(live);
        Self {
            name: name.to_owned(),
            policy,
            shared,
            thread: Mutex::new(None),
            waker: Mutex::new(None),
        }
    }

    /// Starts the controller thread over `workers`. It sleeps while they hold no connection.
    ///
    /// # Panics
    ///
    /// When the operating system cannot start a thread.
    pub fn start(&self, workers: Arc<dyn Workers>) {
        let shared = Arc::clone(&self.shared);
        let policy = self.policy;
        let handle = thread::Builder::new()
            .name(format!("{} scaling", self.name))
            .spawn(move || run(&shared, policy, &*workers))
            .expect("start the worker scaling thread");
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle.thread().clone());
        *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    }

    /// Ends the controller thread and waits for it.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.wake();
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    #[must_use]
    pub fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
            && self
                .thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
    }

    fn wake(&self) {
        if let Some(thread) = &*self.waker.lock().unwrap_or_else(PoisonError::into_inner) {
            thread.unpark();
        }
    }

    /// Whether the controller is parked for want of connections.
    #[must_use]
    pub fn sleeping(&self) -> bool {
        self.shared.sleeping.load(Ordering::SeqCst)
    }

    /// A connection was placed: wakes the controller if it sleeps. Call after counting the connection.
    pub fn connection_placed(&self) {
        if self.sleeping() {
            self.wake();
        }
    }

    /// A connection moved to another worker.
    pub fn note_relocated(&self) {
        self.shared.relocations.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        self.shared.stats()
    }

    /// The first `active()` workers take new connections.
    #[must_use]
    pub fn active(&self) -> usize {
        self.shared.active.load(Ordering::Relaxed)
    }

    /// Sets the active count; the controller thread does it every tick.
    pub fn set_active(&self, active: usize) {
        self.shared.active.store(active, Ordering::Relaxed);
    }

    /// A worker holding more connections than this looks for a lighter one for its idle connections.
    #[must_use]
    pub fn rebalance_above(&self) -> usize {
        self.shared.rebalance_above.load(Ordering::Relaxed)
    }

    pub fn set_rebalance_above(&self, connections: usize) {
        self.shared
            .rebalance_above
            .store(connections, Ordering::Relaxed);
    }

    /// The worker for a new connection: the least loaded active one, the calling worker winning ties when it is
    /// active.
    #[must_use]
    pub fn place(&self, own: usize, load: impl Fn(usize) -> usize) -> usize {
        let active = self.active();
        let start = if own < active { own } else { 0 };
        (0..active)
            .map(|step| (start + step) % active)
            .min_by_key(|worker| load(*worker))
            .unwrap_or(0)
    }

    /// The cheap check of a connection between two requests: its worker is released or holds more than its share.
    #[must_use]
    pub fn wants_relocation(&self, own: usize, own_load: usize) -> bool {
        own >= self.active() || own_load > self.rebalance_above()
    }

    /// The worker the connection of `own` moves to, if any: off a released worker always, off a crowded one only
    /// when another holds clearly fewer connections.
    #[must_use]
    pub fn relocation(&self, own: usize, load: impl Fn(usize) -> usize) -> Option<usize> {
        let target = self.place(own, &load);
        let released = own >= self.active();
        (target != own && (released || load(target) + 1 < load(own))).then_some(target)
    }
}

impl Drop for Scaling {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Per-worker view of what changed since the last tick.
struct Window {
    at: Instant,
    busy: Vec<u64>,
}

fn run(shared: &Shared, policy: Policy, workers: &dyn Workers) {
    let mut controller = Controller::new(policy, shared.workers);
    let start = Instant::now();
    let mut window = Window {
        at: start,
        busy: (0..workers.len())
            .map(|worker| workers.busy_nanos(worker))
            .collect(),
    };
    while !shared.stop.load(Ordering::SeqCst) {
        if sleep_while_unused(shared, workers) {
            window = Window {
                at: Instant::now(),
                busy: (0..workers.len())
                    .map(|worker| workers.busy_nanos(worker))
                    .collect(),
            };
            continue;
        }
        thread::park_timeout(TICK);
        let now = Instant::now();
        // A spurious wake-up leaves a window too short to say anything.
        if now.saturating_duration_since(window.at) < TICK / 2 {
            continue;
        }
        let busy = shares(&mut window, now, workers);
        let active = controller.step(now.saturating_duration_since(start), &busy);
        shared.active.store(active, Ordering::Relaxed);
        shared
            .rebalance_above
            .store(rebalance_above(workers, active), Ordering::Relaxed);
    }
}

/// Parks until a connection is placed. True when it slept.
fn sleep_while_unused(shared: &Shared, workers: &dyn Workers) -> bool {
    let unused = || (0..workers.len()).all(|worker| workers.connections(worker) == 0);
    if !unused() {
        return false;
    }
    // The flag goes up before the connections are counted again, and a placement counts its connection before it
    // reads the flag, so one of the two sees the other.
    shared.sleeping.store(true, Ordering::SeqCst);
    while unused() && !shared.stop.load(Ordering::SeqCst) {
        thread::park();
    }
    shared.sleeping.store(false, Ordering::SeqCst);
    true
}

/// Each worker's busy share since the last tick, in permille.
fn shares(window: &mut Window, now: Instant, workers: &dyn Workers) -> Vec<u32> {
    let wall = u128::from(
        u64::try_from(now.saturating_duration_since(window.at).as_nanos()).unwrap_or(u64::MAX),
    )
    .max(1);
    window.at = now;
    window
        .busy
        .iter_mut()
        .enumerate()
        .map(|(worker, before)| {
            let busy = workers.busy_nanos(worker);
            let spent = u128::from(busy.saturating_sub(*before));
            *before = busy;
            u32::try_from((spent * 1_000 / wall).min(1_000)).unwrap_or(1_000)
        })
        .collect()
}

fn rebalance_above(workers: &dyn Workers, active: usize) -> usize {
    let total = (0..workers.len())
        .map(|worker| workers.connections(worker))
        .sum::<usize>();
    let share = total.div_ceil(active.max(1));
    share + (share / SLACK_DIVISOR).max(MIN_SLACK)
}
