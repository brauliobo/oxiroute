use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
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
    pub fn get(&self, key: &RegistrationKey) -> Option<&Registration> {
        self.records.iter().find(|record| record.key() == *key)
    }

    /// Inserts or replaces a record and reports whether the stored state changed.
    pub fn upsert(&mut self, record: Registration) -> bool {
        match self
            .records
            .iter_mut()
            .find(|stored| stored.key() == record.key())
        {
            Some(stored) if *stored == record => false,
            Some(stored) => {
                *stored = record;
                true
            }
            None => {
                self.records.push(record);
                true
            }
        }
    }

    pub fn remove(&mut self, key: &RegistrationKey) -> Option<Registration> {
        let index = self
            .records
            .iter()
            .position(|record| record.key() == *key)?;
        Some(self.records.remove(index))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("registration store is not accessible: {0}")]
    Io(#[from] io::Error),
    #[error("registration store is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
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
        })
    }

    pub fn load(&self) -> Result<StoredRegistry, StoreError> {
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
        let mut registry = self.load()?;
        let (changed, result) = change(&mut registry);
        if changed {
            registry.seq += 1;
            self.write(&registry)?;
        }
        Ok((result, registry.seq))
    }

    fn write(&self, registry: &StoredRegistry) -> Result<(), StoreError> {
        let path = self.dir.join(STORE_FILE);
        let temp = self.dir.join(format!("{STORE_FILE}.tmp"));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(registry)?)?;
        file.sync_all()?;
        if path.exists() {
            fs::copy(&path, self.dir.join(format!("{STORE_FILE}.bak")))?;
        }
        fs::rename(&temp, &path)?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
