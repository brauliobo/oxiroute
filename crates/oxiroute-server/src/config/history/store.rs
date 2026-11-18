use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use openssl::sha::sha256;

use super::HistoryLimits;
use crate::{
    atomic_file::{ensure_private_directory, write_atomic},
    encoding::lower_hex,
};

const ID_LENGTH: usize = 12;
const MIN_PREFIX: usize = 6;
const ENTRY_MODE: u32 = 0o600;

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryEntry {
    pub(crate) id: String,
    pub(crate) at_unix_ms: u64,
    pub(crate) bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HistoryError {
    #[error("no history entry matches the id")]
    NotFound,
    #[error("the id matches several history entries; give more of it")]
    Ambiguous,
    #[error("the id must be at least {MIN_PREFIX} lowercase hexadecimal digits")]
    Invalid,
    #[error("the history directory is not accessible")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigHistory {
    directory: PathBuf,
    extension: String,
}

impl ConfigHistory {
    pub(crate) fn beside(canonical_path: &Path) -> Self {
        let mut directory = canonical_path.as_os_str().to_owned();
        directory.push(".history");
        Self {
            directory: PathBuf::from(directory),
            extension: canonical_path.extension().map_or_else(
                || "cfg".into(),
                |extension| extension.to_string_lossy().into(),
            ),
        }
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn id_of(text: &[u8]) -> String {
        lower_hex(&sha256(text))[..ID_LENGTH].to_owned()
    }

    pub(crate) fn record(
        &self,
        text: &[u8],
        at_unix_ms: u64,
        limits: HistoryLimits,
    ) -> io::Result<HistoryEntry> {
        ensure_private_directory(&self.directory)?;
        let id = Self::id_of(text);
        for entry in self.entries()?.into_iter().filter(|entry| entry.id == id) {
            self.remove(&entry)?;
        }
        let entry = HistoryEntry {
            id,
            at_unix_ms,
            bytes: text.len() as u64,
        };
        write_atomic(&self.path(&entry), text, ENTRY_MODE)?;
        self.prune(limits)?;
        Ok(entry)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<HistoryEntry>> {
        let mut entries = self.entries()?;
        entries.reverse();
        Ok(entries)
    }

    pub(crate) fn read(&self, prefix: &str) -> Result<(HistoryEntry, Vec<u8>), HistoryError> {
        if prefix.len() < MIN_PREFIX || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(HistoryError::Invalid);
        }
        let prefix = prefix.to_ascii_lowercase();
        let mut matching = self
            .entries()?
            .into_iter()
            .filter(|entry| entry.id.starts_with(&prefix));
        let entry = matching.next().ok_or(HistoryError::NotFound)?;
        if matching.next().is_some() {
            return Err(HistoryError::Ambiguous);
        }
        let text = fs::read(self.path(&entry))?;
        Ok((entry, text))
    }

    fn entries(&self) -> io::Result<Vec<HistoryEntry>> {
        let listing = match fs::read_dir(&self.directory) {
            Ok(listing) => listing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut entries = Vec::new();
        for item in listing {
            let item = item?;
            let Some(entry) =
                self.parse(&item.file_name().to_string_lossy(), item.metadata()?.len())
            else {
                continue;
            };
            entries.push(entry);
        }
        entries
            .sort_by(|left, right| (left.at_unix_ms, &left.id).cmp(&(right.at_unix_ms, &right.id)));
        Ok(entries)
    }

    fn parse(&self, name: &str, bytes: u64) -> Option<HistoryEntry> {
        let stem = name.strip_suffix(&format!(".{}", self.extension))?;
        let (at, id) = stem.split_once('-')?;
        let well_formed = at.len() == 13
            && id.len() == ID_LENGTH
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        let at_unix_ms = at.parse().ok().filter(|_| well_formed)?;
        Some(HistoryEntry {
            id: id.to_owned(),
            at_unix_ms,
            bytes,
        })
    }

    fn path(&self, entry: &HistoryEntry) -> PathBuf {
        self.directory.join(format!(
            "{:013}-{}.{}",
            entry.at_unix_ms, entry.id, self.extension
        ))
    }

    fn remove(&self, entry: &HistoryEntry) -> io::Result<()> {
        fs::remove_file(self.path(entry))
    }

    fn prune(&self, limits: HistoryLimits) -> io::Result<()> {
        let mut kept_bytes = 0_u64;
        for (index, entry) in self.list()?.into_iter().enumerate() {
            kept_bytes = kept_bytes.saturating_add(entry.bytes);
            if index >= limits.max_entries || kept_bytes > limits.max_total_bytes {
                self.remove(&entry)?;
            }
        }
        Ok(())
    }
}
