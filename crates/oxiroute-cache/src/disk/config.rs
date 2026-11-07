#[allow(clippy::wildcard_imports)]
use super::*;

/// Persistent-store limits. `max_disk_bytes` and `max_disk_files` count recognized, durably
/// published cache records exactly; unrelated root entries are excluded. Admission reserves the
/// new record before creating one bounded, unpublished temporary record. During no-replace
/// publication, the temp and final names briefly refer to that same inode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskCacheConfig {
    pub memory: CacheConfig,
    pub max_disk_bytes: u64,
    pub max_disk_files: usize,
    pub max_record_bytes: usize,
}

impl Default for DiskCacheConfig {
    fn default() -> Self {
        Self {
            memory: CacheConfig::default(),
            max_disk_bytes: 512 * 1024 * 1024,
            max_disk_files: 10_000,
            max_record_bytes: 9 * 1024 * 1024,
        }
    }
}

/// Quota and locking scope of [`DiskCache`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskQuotaScope {
    /// One advisory, descriptor-held lease covers the root. A second cooperating `DiskCache`
    /// instance in this or another process is rejected, making its counters root-global. File mode
    /// and ownership exclude other users; a non-cooperating process running as the same user can
    /// bypass the advisory lease, so quotas are not distributed against such a process.
    ExclusiveRoot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiskCacheStats {
    pub memory: CacheStats,
    pub disk_entries: usize,
    pub disk_bytes: u64,
    pub recovered: u64,
    pub stale_temps_removed: u64,
    pub corrupt_records_removed: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DiskCacheError {
    #[error("invalid persistent cache configuration")]
    InvalidConfig,
    #[error("persistent cache root cannot be opened or created without following symlinks")]
    RootOpen(#[source] io::Error),
    #[error("persistent cache root must be owned by this user and have mode 0700")]
    RootControl,
    #[error("persistent cache ownership lock is unsafe or cannot be opened")]
    OwnershipOpen(#[source] io::Error),
    #[error("persistent cache root is already owned by another instance")]
    AlreadyOwned,
    #[error("persistent cache ownership lock cannot be acquired")]
    OwnershipLock(#[source] io::Error),
    #[error("persistent cache root cannot be enumerated")]
    RootRead(#[source] io::Error),
    #[error("persistent cache record filesystem operation failed")]
    Storage(#[source] io::Error),
    #[error("persistent cache record exceeds configured disk bounds")]
    RecordTooLarge,
    #[error("persistent cache record cannot fit the configured quota")]
    Quota,
    #[error("persistent cache namespace contains an entry that cannot be safely removed")]
    UnsafeEntry,
    #[error("persistent cache is shut down")]
    Closed,
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[cfg(test)]
    #[error("injected persistent cache fault")]
    Injected,
}
