#[allow(clippy::wildcard_imports)]
use super::*;

enum DiskBackendRegistryEntry {
    Opening {
        insertion: u64,
        config: DiskCacheConfig,
    },
    Ready {
        insertion: u64,
        config: DiskCacheConfig,
        backend: Weak<DiskBackend>,
    },
}

struct DiskBackendRegistry {
    entries: Mutex<HashMap<PathBuf, DiskBackendRegistryEntry>>,
    changed: Condvar,
}

static DISK_BACKEND_REGISTRY: OnceLock<DiskBackendRegistry> = OnceLock::new();
static NEXT_DISK_BACKEND_INSERTION: AtomicU64 = AtomicU64::new(1);

pub(crate) struct DiskBackendRegistryLease {
    root: PathBuf,
    insertion: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DiskBackendOpenError {
    Unavailable,
    TimedOut,
}

impl Drop for DiskBackendRegistryLease {
    fn drop(&mut self) {
        let Some(registry) = DISK_BACKEND_REGISTRY.get() else {
            return;
        };
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.get(&self.root).is_some_and(|entry| match entry {
            DiskBackendRegistryEntry::Opening { insertion, .. }
            | DiskBackendRegistryEntry::Ready { insertion, .. } => *insertion == self.insertion,
        }) {
            entries.remove(&self.root);
            registry.changed.notify_all();
        }
    }
}

#[cfg(test)]
pub(super) fn open_shared_disk_backend(
    root: &std::path::Path,
    config: &DiskCacheConfig,
) -> Result<Arc<DiskBackend>, ()> {
    open_shared_disk_backend_with_deadline(root, config, None).map_err(|_| ())
}

#[expect(
    clippy::too_many_lines,
    reason = "the token-CAS loop keeps one registry transition state machine auditable"
)]
pub(super) fn open_shared_disk_backend_with_deadline(
    root: &std::path::Path,
    config: &DiskCacheConfig,
    deadline: Option<Instant>,
) -> Result<Arc<DiskBackend>, DiskBackendOpenError> {
    let registry = DISK_BACKEND_REGISTRY.get_or_init(|| DiskBackendRegistry {
        entries: Mutex::new(HashMap::new()),
        changed: Condvar::new(),
    });
    loop {
        let snapshot = {
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                match entries.get(root) {
                    Some(DiskBackendRegistryEntry::Opening {
                        insertion,
                        config: opening_config,
                    }) => {
                        if opening_config != config {
                            return Err(DiskBackendOpenError::Unavailable);
                        }
                        let observed = *insertion;
                        let same_opening = |entries: &mut HashMap<_, _>| {
                            entries.get(root).is_some_and(|entry| {
                                matches!(
                                    entry,
                                    DiskBackendRegistryEntry::Opening { insertion, .. }
                                        if *insertion == observed
                                )
                            })
                        };
                        if let Some(deadline) = deadline {
                            let now = Instant::now();
                            if now >= deadline {
                                return Err(DiskBackendOpenError::TimedOut);
                            }
                            let (next, _) = registry
                                .changed
                                .wait_timeout_while(
                                    entries,
                                    deadline.saturating_duration_since(now),
                                    same_opening,
                                )
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            entries = next;
                            if same_opening(&mut entries) && Instant::now() >= deadline {
                                return Err(DiskBackendOpenError::TimedOut);
                            }
                        } else {
                            entries = registry
                                .changed
                                .wait_while(entries, same_opening)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                    }
                    Some(DiskBackendRegistryEntry::Ready {
                        insertion,
                        config: existing_config,
                        backend,
                    }) => {
                        break Some((*insertion, existing_config.clone(), backend.clone()));
                    }
                    None => {
                        let insertion = NEXT_DISK_BACKEND_INSERTION.fetch_add(1, Ordering::Relaxed);
                        entries.insert(
                            root.to_owned(),
                            DiskBackendRegistryEntry::Opening {
                                insertion,
                                config: config.clone(),
                            },
                        );
                        break None;
                    }
                }
            }
        };

        if let Some((insertion, existing_config, backend)) = snapshot {
            #[cfg(test)]
            run_disk_registry_snapshot_hook(root);
            let existing = backend.upgrade();
            if let Some(existing) = existing {
                return (&existing_config == config)
                    .then_some(existing)
                    .ok_or(DiskBackendOpenError::Unavailable);
            }
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries.get(root).is_some_and(|entry| {
                matches!(
                    entry,
                    DiskBackendRegistryEntry::Ready {
                        insertion: current,
                        ..
                    } if *current == insertion
                )
            }) {
                entries.remove(root);
                registry.changed.notify_all();
            }
            continue;
        }

        let insertion = {
            let entries = registry
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match entries.get(root) {
                Some(DiskBackendRegistryEntry::Opening { insertion, .. }) => *insertion,
                Some(DiskBackendRegistryEntry::Ready { .. }) | None => continue,
            }
        };
        #[cfg(test)]
        run_disk_registry_opening_hook(root);
        let Ok(cache) = DiskCache::open(root, config.clone()) else {
            remove_disk_registry_opening(registry, root, insertion);
            return Err(DiskBackendOpenError::Unavailable);
        };
        let cache = Arc::new(cache);
        let backend = Arc::new(DiskBackend::new(
            cache,
            DiskBackendRegistryLease {
                root: root.to_owned(),
                insertion,
            },
        ));
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owns_opening = entries.get(root).is_some_and(|entry| {
            matches!(
                entry,
                DiskBackendRegistryEntry::Opening {
                    insertion: current,
                    ..
                } if *current == insertion
            )
        });
        if owns_opening {
            entries.insert(
                root.to_owned(),
                DiskBackendRegistryEntry::Ready {
                    insertion,
                    config: config.clone(),
                    backend: Arc::downgrade(&backend),
                },
            );
            registry.changed.notify_all();
            drop(entries);
            return Ok(backend);
        }
        drop(entries);
        drop(backend);
    }
}

fn remove_disk_registry_opening(
    registry: &DiskBackendRegistry,
    root: &std::path::Path,
    insertion: u64,
) {
    let mut entries = registry
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if entries.get(root).is_some_and(|entry| {
        matches!(
            entry,
            DiskBackendRegistryEntry::Opening {
                insertion: current,
                ..
            } if *current == insertion
        )
    }) {
        entries.remove(root);
        registry.changed.notify_all();
    }
}

#[cfg(test)]
struct DiskRegistrySnapshotHook {
    root: PathBuf,
    reached: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
static DISK_REGISTRY_SNAPSHOT_HOOK: Mutex<Option<Arc<DiskRegistrySnapshotHook>>> = Mutex::new(None);

#[cfg(test)]
static DISK_REGISTRY_OPENING_HOOKS: OnceLock<
    Mutex<HashMap<PathBuf, Arc<DiskRegistrySnapshotHook>>>,
> = OnceLock::new();

#[cfg(test)]
fn run_disk_registry_snapshot_hook(root: &std::path::Path) {
    let hook = DISK_REGISTRY_SNAPSHOT_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|hook| hook.root == root)
        .cloned();
    if let Some(hook) = hook {
        hook.reached.wait();
        hook.release.wait();
    }
}

#[cfg(test)]
fn run_disk_registry_opening_hook(root: &std::path::Path) {
    let hook = DISK_REGISTRY_OPENING_HOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .cloned();
    if let Some(hook) = hook {
        hook.reached.wait();
        hook.release.wait();
    }
}

#[cfg(test)]
pub(super) struct DiskRegistrySnapshotHookGuard;

#[cfg(test)]
pub(crate) struct DiskRegistryOpeningHookGuard {
    root: PathBuf,
}

#[cfg(test)]
impl Drop for DiskRegistrySnapshotHookGuard {
    fn drop(&mut self) {
        DISK_REGISTRY_SNAPSHOT_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
impl Drop for DiskRegistryOpeningHookGuard {
    fn drop(&mut self) {
        DISK_REGISTRY_OPENING_HOOKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.root);
    }
}

#[cfg(test)]
pub(super) fn install_disk_registry_snapshot_hook(
    root: PathBuf,
) -> (
    DiskRegistrySnapshotHookGuard,
    Arc<std::sync::Barrier>,
    Arc<std::sync::Barrier>,
) {
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook = Arc::new(DiskRegistrySnapshotHook {
        root,
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    let replaced = DISK_REGISTRY_SNAPSHOT_HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(hook);
    assert!(replaced.is_none(), "disk registry hook already installed");
    (DiskRegistrySnapshotHookGuard, reached, release)
}

#[cfg(test)]
pub(crate) fn install_disk_registry_opening_hook(
    root: PathBuf,
) -> (
    DiskRegistryOpeningHookGuard,
    Arc<std::sync::Barrier>,
    Arc<std::sync::Barrier>,
) {
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let hook = Arc::new(DiskRegistrySnapshotHook {
        root: root.clone(),
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    let replaced = DISK_REGISTRY_OPENING_HOOKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.clone(), hook);
    assert!(
        replaced.is_none(),
        "disk registry opening hook already installed"
    );
    (DiskRegistryOpeningHookGuard { root }, reached, release)
}

#[cfg(test)]
pub(crate) fn disk_backend_registry_contains(root: &std::path::Path) -> bool {
    DISK_BACKEND_REGISTRY.get().is_some_and(|registry| {
        registry
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(root)
    })
}
