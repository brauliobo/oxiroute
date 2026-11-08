#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ConfigReloadMonitor {
    filter: ConfigEventFilter,
    watcher: RecommendedWatcher,
    watched_directories: HashSet<PathBuf>,
    events: Receiver<()>,
    next_reconciliation: Instant,
    first_event: Option<Instant>,
    last_event: Option<Instant>,
}

impl ConfigReloadMonitor {
    pub(super) fn start(
        path: &Path,
        dependencies: &[PathBuf],
        waker: Arc<Waker>,
    ) -> notify::Result<Self> {
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let (wake, events) = mpsc::sync_channel(1);
        let watcher_wake = wake.clone();
        let filter = ConfigEventFilter::new(path, dependencies);
        let watcher_filter = filter.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let relevant = match event {
                    Err(_) => true,
                    Ok(event) => {
                        event.need_rescan()
                            || (!event.kind.is_access() && watcher_filter.matches(&event))
                    }
                };
                if relevant {
                    let _ = watcher_wake.try_send(());
                    waker.wake();
                }
            },
            notify::Config::default(),
        )?;
        watcher.watch(&parent, RecursiveMode::NonRecursive)?;
        let mut monitor = Self {
            filter,
            watcher,
            watched_directories: HashSet::from([parent]),
            events,
            next_reconciliation: Instant::now(),
            first_event: None,
            last_event: None,
        };
        monitor.watch_dependencies(dependencies)?;
        Ok(monitor)
    }

    pub(super) fn watch_dependencies(&mut self, dependencies: &[PathBuf]) -> notify::Result<()> {
        self.filter.set_dependencies(dependencies);
        for dependency in dependencies {
            let Some(parent) = dependency
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
            else {
                continue;
            };
            let parent = parent.to_path_buf();
            if self.watched_directories.insert(parent.clone()) {
                self.watcher.watch(&parent, RecursiveMode::NonRecursive)?;
            }
        }
        Ok(())
    }

    /// When `next_trigger` next has something to report without a new file event.
    pub(super) fn next_deadline(&self) -> Instant {
        match (self.first_event, self.last_event) {
            (Some(first), Some(last)) => {
                (last + CONFIG_RELOAD_DEBOUNCE).min(first + CONFIG_RELOAD_MAX_DEBOUNCE)
            }
            _ => self.next_reconciliation,
        }
    }

    pub(super) fn next_trigger(&mut self, now: Instant) -> bool {
        if self.events.try_iter().next().is_some() {
            self.first_event.get_or_insert(now);
            self.last_event = Some(now);
        }
        if let (Some(first_event), Some(last_event)) = (self.first_event, self.last_event) {
            let quiet = now.saturating_duration_since(last_event) >= CONFIG_RELOAD_DEBOUNCE;
            let bounded = now.saturating_duration_since(first_event) >= CONFIG_RELOAD_MAX_DEBOUNCE;
            if quiet || bounded {
                self.first_event = None;
                self.last_event = None;
                self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
                return true;
            }
            return false;
        }
        let periodic = now >= self.next_reconciliation;
        if periodic {
            self.next_reconciliation = now + CONFIG_RECONCILIATION_INTERVAL;
        }
        periodic
    }
}

pub(super) struct SignalMonitor {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) reload: Arc<AtomicBool>,
    pub(super) waker: Arc<Waker>,
    pub(super) handle: Handle,
    pub(super) thread: JoinHandle<()>,
}

impl SignalMonitor {
    pub(super) fn new(waker: Arc<Waker>) -> Result<Self, Box<dyn Error>> {
        let stop = Arc::new(AtomicBool::new(false));
        let reload = Arc::new(AtomicBool::new(false));
        let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
        let handle = signals.handle();
        let signal_stop = Arc::clone(&stop);
        let signal_reload = Arc::clone(&reload);
        let signal_waker = Arc::clone(&waker);
        let thread = thread::Builder::new()
            .name("oxiroute-master-signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    match signal {
                        SIGTERM | SIGINT => {
                            signal_stop.store(true, Ordering::Release);
                            signal_waker.wake();
                            break;
                        }
                        SIGHUP => {
                            signal_reload.store(true, Ordering::Release);
                            signal_waker.wake();
                        }
                        _ => {}
                    }
                }
            })?;
        Ok(Self {
            stop,
            reload,
            waker,
            handle,
            thread,
        })
    }

    /// Sleeps until a worker event, a signal, a file event or `due`; the master's own deadlines also end it.
    pub(super) fn wait(&self, master: &Master, due: Option<Instant>) -> io::Result<()> {
        master.wait(
            &[self.waker.descriptor()],
            due.map(|due| due.saturating_duration_since(Instant::now())),
        )?;
        self.waker.drain();
        Ok(())
    }

    pub(super) fn finish(self, result: Result<(), Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
        let Self {
            stop: _,
            reload: _,
            waker: _,
            handle,
            thread,
        } = self;
        handle.close();
        let signal_result = thread.join();
        match (result, signal_result) {
            (Err(error), _) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(_)) => {
                Err(io::Error::other("master signal thread terminated unexpectedly").into())
            }
        }
    }
}
