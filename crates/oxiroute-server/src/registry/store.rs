use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};

use super::record::{Registration, RegistrationKey};

const STORE_FILE: &str = "registrations.json";
const LOCK_FILE: &str = "registrations.lock";
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

/// The persisted registry: a monotonic revision and the records, without lease deadlines.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredRegistry {
    pub seq: u64,
    pub records: Vec<Registration>,
}

impl StoredRegistry {
    fn position(&self, key: &RegistrationKey) -> Option<usize> {
        self.records
            .iter()
            .position(|record| record.pool == key.pool && record.name == key.name)
    }

    pub fn get(&self, key: &RegistrationKey) -> Option<&Registration> {
        self.position(key).map(|index| &self.records[index])
    }

    /// Inserts or replaces a record and reports whether the stored state changed.
    pub fn upsert(&mut self, record: Registration) -> bool {
        match self.position(&record.key()) {
            Some(index) if self.records[index] == record => false,
            Some(index) => {
                self.records[index] = record;
                true
            }
            None => {
                self.records.push(record);
                true
            }
        }
    }

    pub fn remove(&mut self, key: &RegistrationKey) -> Option<Registration> {
        let index = self.position(key)?;
        Some(self.records.remove(index))
    }

    /// Removes every record named by `keys` and reports how many it held.
    pub fn remove_all(&mut self, keys: &[RegistrationKey]) -> usize {
        let doomed = keys
            .iter()
            .map(|key| (key.pool.as_str(), key.name.as_str()))
            .collect::<HashSet<_>>();
        let before = self.records.len();
        self.records
            .retain(|record| !doomed.contains(&(record.pool.as_str(), record.name.as_str())));
        before - self.records.len()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("registration store is not accessible: {0}")]
    Io(#[from] io::Error),
    #[error("registration store is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
}

/// Identifies one version of the store file: every write replaces it, so its inode and times change.
type Stamp = (u64, i64, i64, u64);

#[derive(Debug)]
struct Cached {
    stamp: Option<Stamp>,
    registry: Arc<StoredRegistry>,
}

/// The persisted registry. The parsed records stay in memory and are read again only after the
/// file was replaced, which another process may do.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    cache: Arc<Mutex<Option<Cached>>>,
}

impl Store {
    /// Opens the store directory, creating it private to the service account.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            cache: Arc::default(),
        })
    }

    fn stamp(&self) -> Result<Option<Stamp>, StoreError> {
        match fs::metadata(self.dir.join(STORE_FILE)) {
            Ok(meta) => Ok(Some((
                meta.ino(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.len(),
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Option<Cached>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The registry as of the file now, parsed only when the file changed since the last read.
    pub fn load(&self) -> Result<Arc<StoredRegistry>, StoreError> {
        let registry = self.take()?;
        self.keep(&registry)?;
        Ok(registry)
    }

    /// Takes the cached registry out, so the caller alone holds it, or parses the file anew.
    fn take(&self) -> Result<Arc<StoredRegistry>, StoreError> {
        let stamp = self.stamp()?;
        let cached = self.cache().take();
        match cached {
            Some(cached) if cached.stamp == stamp => Ok(cached.registry),
            _ => Ok(Arc::new(self.read()?)),
        }
    }

    /// Caches `registry` as the content of the file now.
    fn keep(&self, registry: &Arc<StoredRegistry>) -> Result<(), StoreError> {
        let stamp = self.stamp()?;
        *self.cache() = Some(Cached {
            stamp,
            registry: Arc::clone(registry),
        });
        Ok(())
    }

    fn read(&self) -> Result<StoredRegistry, StoreError> {
        let mut bytes = Vec::new();
        match File::open(self.dir.join(STORE_FILE)) {
            Ok(file) => file.take(MAX_STORE_BYTES).read_to_end(&mut bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StoredRegistry::default());
            }
            Err(error) => return Err(error.into()),
        };
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Reads, changes and atomically replaces the store under an exclusive lock. `change` returns
    /// whether it modified anything; only then is the revision bumped and the file rewritten.
    pub fn update<R>(
        &self,
        change: impl FnOnce(&mut StoredRegistry) -> (bool, R),
    ) -> Result<(R, u64), StoreError> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.dir.join(LOCK_FILE))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(io::Error::from)?;
        let mut registry = self.take()?;
        let (changed, result) = change(Arc::make_mut(&mut registry));
        if changed {
            Arc::make_mut(&mut registry).seq += 1;
            self.write(&registry)?;
        }
        self.keep(&registry)?;
        Ok((result, registry.seq))
    }

    fn write(&self, registry: &Arc<StoredRegistry>) -> Result<(), StoreError> {
        let path = self.dir.join(STORE_FILE);
        let temp = self.dir.join(format!("{STORE_FILE}.tmp"));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(&**registry)?)?;
        file.sync_all()?;
        self.keep_backup(&path)?;
        fs::rename(&temp, &path)?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Keeps the file about to be replaced as `.bak` by linking it, which copies nothing.
    fn keep_backup(&self, path: &Path) -> Result<(), StoreError> {
        let backup = self.dir.join(format!("{STORE_FILE}.bak"));
        let link = self.dir.join(format!("{STORE_FILE}.bak.tmp"));
        match fs::remove_file(&link) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        match fs::hard_link(path, &link) {
            Ok(()) => Ok(fs::rename(&link, backup)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests;
