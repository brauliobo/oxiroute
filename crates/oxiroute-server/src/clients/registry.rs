use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use super::{
    fold::History,
    key::IpKey,
    stats::{Arrival, ConnStats},
};
use crate::sharded::{self, Sharded};

pub(crate) const DEFAULT_MAX_CLIENTS: u64 = 10_000;
pub(crate) const DEFAULT_IPV6_PREFIX: u8 = 128;
const SEQUENCE_BITS: u32 = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) max_clients: u64,
    pub(crate) ipv6_prefix: u8,
}

impl From<oxiroute_config::ClientTrackingSettings> for Settings {
    fn from(settings: oxiroute_config::ClientTrackingSettings) -> Self {
        Self {
            enabled: settings.enabled,
            max_clients: settings.max_clients,
            ipv6_prefix: settings.ipv6_prefix,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_clients: DEFAULT_MAX_CLIENTS,
            ipv6_prefix: DEFAULT_IPV6_PREFIX,
        }
    }
}

struct Applied {
    enabled: AtomicBool,
    max_clients: AtomicU64,
    ipv6_prefix: AtomicU8,
}

impl Applied {
    fn new(settings: Settings) -> Self {
        Self {
            enabled: AtomicBool::new(settings.enabled),
            max_clients: AtomicU64::new(settings.max_clients),
            ipv6_prefix: AtomicU8::new(settings.ipv6_prefix),
        }
    }

    fn store(&self, settings: Settings) {
        self.max_clients
            .store(settings.max_clients, Ordering::Relaxed);
        self.ipv6_prefix
            .store(settings.ipv6_prefix, Ordering::Relaxed);
        self.enabled.store(settings.enabled, Ordering::Release);
    }

    fn load(&self) -> Settings {
        Settings {
            enabled: self.enabled.load(Ordering::Acquire),
            max_clients: self.max_clients.load(Ordering::Relaxed),
            ipv6_prefix: self.ipv6_prefix.load(Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
pub(super) struct Shard {
    live: Vec<Option<Arc<ConnStats>>>,
    free: Vec<u32>,
    sequence: u64,
    pub(super) history: History,
}

impl Shard {
    pub(super) fn live(&self) -> impl Iterator<Item = &Arc<ConnStats>> {
        self.live.iter().flatten()
    }

    fn insert(&mut self, index: usize, arrival: Arrival, key: IpKey) -> Arc<ConnStats> {
        let slot = self.free.pop().unwrap_or_else(|| {
            u32::try_from(self.live.len()).expect("fewer than 2^32 live connections per thread")
        });
        let id = (index as u64) << SEQUENCE_BITS | self.sequence;
        self.sequence += 1;
        let stats = Arc::new(ConnStats::new(arrival, key, (index, slot, id)));
        match self.live.get_mut(slot as usize) {
            Some(cell) => *cell = Some(Arc::clone(&stats)),
            None => self.live.push(Some(Arc::clone(&stats))),
        }
        stats
    }

    fn release(&mut self, stats: &ConnStats) {
        let slot = stats.slot() as usize;
        self.live[slot] = None;
        self.free.push(stats.slot());
    }
}

pub(crate) struct Registry {
    applied: Applied,
    shards: Sharded<Mutex<Shard>>,
}

impl Registry {
    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            applied: Applied::new(settings),
            shards: Sharded::new(Mutex::default),
        }
    }

    pub(crate) fn activate(&self, settings: Settings) {
        self.applied.store(settings);
    }

    pub(crate) fn settings(&self) -> Settings {
        self.applied.load()
    }

    pub(super) fn lock(&self, index: usize) -> MutexGuard<'_, Shard> {
        self.shards
            .get(index)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts tracking a connection of the calling thread; `None` while tracking is disabled.
    pub(crate) fn open(self: &Arc<Self>, arrival: Arrival) -> Option<Connection> {
        let settings = self.applied.load();
        if !settings.enabled {
            return None;
        }
        let key = IpKey::new(arrival.peer.ip(), settings.ipv6_prefix);
        let index = sharded::local_index();
        let stats = self.lock(index).insert(index, arrival, key);
        Some(Connection {
            stats,
            registry: Arc::clone(self),
        })
    }

    fn close(&self, stats: &ConnStats) {
        let capacity = usize::try_from(self.applied.load().max_clients).unwrap_or(usize::MAX);
        let mut shard = self.lock(stats.shard());
        shard.release(stats);
        shard.history.fold(stats, capacity);
    }
}

/// The registration of one connection; dropping it folds the connection into its client's totals.
pub(crate) struct Connection {
    stats: Arc<ConnStats>,
    registry: Arc<Registry>,
}

impl Connection {
    pub(crate) const fn stats(&self) -> &Arc<ConnStats> {
        &self.stats
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.registry.close(&self.stats);
    }
}
