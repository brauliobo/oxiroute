//! Static files kept for one validity window: small ones in memory, larger ones as an open descriptor.
//!
//! A request inside the window is answered from the entry with no path walk, no `open`, no `stat` and no
//! blocking-pool hop, and the fast lane writes the response head the entry rendered when it was filled. After the
//! window the next request reads the file again, so an edit, replacement or removal is visible within
//! [`VALIDITY`]. An entry holds the file it was filled from: a file replaced inside the window is still served as it
//! was, whole and with the validators it had. Only the request path is the key: path safety is decided when the entry
//! is filled.

use super::*;

pub(crate) const VALIDITY: Duration = Duration::from_secs(1);
const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Descriptors held open at once, so a directory of large files cannot use up the process's.
const MAX_OPEN: usize = 128;

/// A file as the cache holds it, and the head of its plain `200` response on an HTTP/1 connection.
#[derive(Debug)]
pub(crate) struct CachedFile {
    pub(crate) file: StaticFile,
    /// `None` when the head cannot be rendered: only the generic writer serves the file.
    pub(crate) head: Option<Bytes>,
    filled: Instant,
}

impl CachedFile {
    fn fresh(&self, now: Instant, validity: Duration) -> bool {
        now.saturating_duration_since(self.filled) < validity
    }

    fn memory_bytes(&self) -> usize {
        match &self.file.content {
            StaticContent::Memory(body) => body.len(),
            StaticContent::Open { .. } => 0,
        }
    }

    const fn is_open(&self) -> bool {
        matches!(self.file.content, StaticContent::Open { .. })
    }
}

#[derive(Debug)]
pub(crate) struct StaticCache {
    entries: Box<Mutex<Entries>>,
    validity: Duration,
}

impl Default for StaticCache {
    fn default() -> Self {
        Self::new(VALIDITY)
    }
}

#[derive(Debug, Default)]
struct Entries {
    files: HashMap<Box<str>, Arc<CachedFile>>,
    bytes: usize,
    open: usize,
}

impl Entries {
    fn remove(&mut self, path: &str) {
        if let Some(old) = self.files.remove(path) {
            self.forget(&old);
        }
    }

    fn forget(&mut self, entry: &CachedFile) {
        self.bytes -= entry.memory_bytes();
        self.open -= usize::from(entry.is_open());
    }

    fn drop_expired(&mut self, now: Instant, validity: Duration) {
        self.files.retain(|_, entry| entry.fresh(now, validity));
        self.bytes = self.files.values().map(|entry| entry.memory_bytes()).sum();
        self.open = self.files.values().filter(|entry| entry.is_open()).count();
    }

    fn has_room(&self, file: &CachedFile) -> bool {
        self.files.len() < MAX_ENTRIES
            && self.bytes + file.memory_bytes() <= MAX_BYTES
            && self.open + usize::from(file.is_open()) <= MAX_OPEN
    }
}

impl StaticCache {
    pub(crate) fn new(validity: Duration) -> Self {
        Self {
            entries: Box::default(),
            validity,
        }
    }

    pub(crate) fn get(&self, path: &str, now: Instant) -> Option<Arc<CachedFile>> {
        let entries = self.lock();
        entries
            .files
            .get(path)
            .filter(|entry| entry.fresh(now, self.validity))
            .cloned()
    }

    /// Remembers a file; a full cache drops expired entries first and otherwise skips it.
    pub(crate) fn insert(&self, path: &str, file: &StaticFile, head: Option<Bytes>, now: Instant) {
        let entry = CachedFile {
            file: file.clone(),
            head,
            filled: now,
        };
        let mut entries = self.lock();
        entries.remove(path);
        if !entries.has_room(&entry) {
            entries.drop_expired(now, self.validity);
        }
        if !entries.has_room(&entry) {
            return;
        }
        entries.bytes += entry.memory_bytes();
        entries.open += usize::from(entry.is_open());
        entries.files.insert(path.into(), Arc::new(entry));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
