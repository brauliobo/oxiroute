//! Small static files kept in memory for one validity window.
//!
//! A request inside the window is answered from memory with no file access and no blocking-pool hop. After the
//! window the next request reads the file again, so an edit, replacement or removal is visible within
//! [`VALIDITY`]. Only the request path is the key: path safety is decided when the entry is filled.

use super::*;

pub(crate) const VALIDITY: Duration = Duration::from_secs(1);
const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct StaticCache {
    entries: Box<Mutex<Entries>>,
}

#[derive(Debug, Default)]
struct Entries {
    files: HashMap<Box<str>, Entry>,
    bytes: usize,
}

#[derive(Debug)]
struct Entry {
    body: Bytes,
    content_type: HeaderValue,
    etag: HeaderValue,
    content_length: HeaderValue,
    last_modified: HeaderValue,
    modified: SystemTime,
    name: Arc<OsStr>,
    size: u64,
    filled: Instant,
}

impl Entry {
    fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.filled) < VALIDITY
    }

    fn file(&self) -> StaticFile {
        StaticFile {
            content: StaticContent::Memory(self.body.clone()),
            content_type: self.content_type.clone(),
            etag: self.etag.clone(),
            content_length: self.content_length.clone(),
            last_modified: self.last_modified.clone(),
            modified: self.modified,
            name: self.name.clone(),
            size: self.size,
        }
    }
}

impl StaticCache {
    pub(crate) fn get(&self, path: &str, now: Instant) -> Option<StaticFile> {
        let entries = self.lock();
        entries
            .files
            .get(path)
            .filter(|entry| entry.fresh(now))
            .map(Entry::file)
    }

    /// Remembers a file that was read whole; a full cache drops expired entries first and otherwise skips it.
    pub(crate) fn insert(&self, path: &str, file: &StaticFile, now: Instant) {
        let StaticContent::Memory(body) = &file.content else {
            return;
        };
        let mut entries = self.lock();
        if let Some(old) = entries.files.remove(path) {
            entries.bytes -= old.body.len();
        }
        if entries.files.len() >= MAX_ENTRIES || entries.bytes + body.len() > MAX_BYTES {
            entries.files.retain(|_, entry| entry.fresh(now));
            entries.bytes = entries.files.values().map(|entry| entry.body.len()).sum();
        }
        if entries.files.len() >= MAX_ENTRIES || entries.bytes + body.len() > MAX_BYTES {
            return;
        }
        entries.bytes += body.len();
        entries.files.insert(
            path.into(),
            Entry {
                body: body.clone(),
                content_type: file.content_type.clone(),
                etag: file.etag.clone(),
                content_length: file.content_length.clone(),
                last_modified: file.last_modified.clone(),
                modified: file.modified,
                name: file.name.clone(),
                size: file.size,
                filled: now,
            },
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
