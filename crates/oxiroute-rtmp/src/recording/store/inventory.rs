use std::{fmt::Write as _, io::Read};

use sha2::{Digest, Sha256};

#[allow(clippy::wildcard_imports)]
use super::*;

const SIDECAR_HEADER: &str = "oxiroute-recording-meta 1";
const MAX_SIDECAR_BYTES: u64 = 4096;
const ID_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingStatus {
    Recording,
    Complete,
    Partial,
}

/// Metadata the recorder writes next to a recording, read back by the inventory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordingMeta {
    pub complete: bool,
    pub stream: Option<String>,
    pub started_unix_ms: Option<u64>,
    pub ended_unix_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingEntry {
    pub id: String,
    pub name: String,
    pub status: RecordingStatus,
    pub size_bytes: u64,
    pub modified_unix_ms: u64,
    pub stream: Option<String>,
    pub started_unix_ms: Option<u64>,
    pub ended_unix_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

pub(super) fn id_scope(root: &Path) -> [u8; 8] {
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let mut scope = [0; 8];
    scope.copy_from_slice(&digest[..8]);
    scope
}

fn recording_id(scope: [u8; 8], name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope);
    hasher.update([0]);
    hasher.update(name.as_bytes());
    hasher.finalize()[..ID_BYTES].iter().fold(
        String::with_capacity(ID_BYTES * 2),
        |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        },
    )
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match (character, characters.clone().next()) {
            ('\\', Some('n')) => {
                text.push('\n');
                characters.next();
            }
            ('\\', Some('\\')) => {
                text.push('\\');
                characters.next();
            }
            _ => text.push(character),
        }
    }
    text
}

impl RecordingMeta {
    pub(super) fn encode(&self) -> String {
        let mut text = format!("{SIDECAR_HEADER}\ncomplete={}\n", u8::from(self.complete));
        let mut field = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                let _ = writeln!(text, "{key}={value}");
            }
        };
        field("stream", self.stream.as_deref().map(escape));
        field(
            "started_unix_ms",
            self.started_unix_ms.map(|v| v.to_string()),
        );
        field("ended_unix_ms", self.ended_unix_ms.map(|v| v.to_string()));
        field("duration_ms", self.duration_ms.map(|v| v.to_string()));
        field("video_codec", self.video_codec.clone());
        field("audio_codec", self.audio_codec.clone());
        text
    }

    pub(super) fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != SIDECAR_HEADER {
            return None;
        }
        let mut meta = Self::default();
        for line in lines {
            let (key, value) = line.split_once('=')?;
            match key {
                "complete" => meta.complete = value == "1",
                "stream" => meta.stream = Some(unescape(value)),
                "started_unix_ms" => meta.started_unix_ms = value.parse().ok(),
                "ended_unix_ms" => meta.ended_unix_ms = value.parse().ok(),
                "duration_ms" => meta.duration_ms = value.parse().ok(),
                "video_codec" => meta.video_codec = Some(value.to_owned()),
                "audio_codec" => meta.audio_codec = Some(value.to_owned()),
                _ => {}
            }
        }
        Some(meta)
    }
}

fn read_sidecar(root: &OwnedFd, name: &str) -> Option<RecordingMeta> {
    let descriptor = rustix_fs::openat(
        root,
        sidecar_name(name),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .ok()?;
    let mut text = String::new();
    File::from(descriptor)
        .take(MAX_SIDECAR_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    RecordingMeta::decode(&text)
}

fn modified_unix_ms(metadata: &rustix_fs::Stat) -> u64 {
    let seconds = u64::try_from(metadata.st_mtime).unwrap_or(0);
    let nanoseconds = metadata.st_mtime_nsec;
    seconds
        .saturating_mul(1000)
        .saturating_add(nanoseconds / 1_000_000)
}

impl RecordingStore {
    /// Writes the metadata sidecar of one recording. The inventory also works from the file alone,
    /// so a recording whose sidecar cannot be written is still listed.
    ///
    /// # Errors
    ///
    /// Returns an error when the sidecar cannot be created or written.
    pub(crate) fn write_sidecar(
        &self,
        name: &str,
        meta: &RecordingMeta,
    ) -> Result<(), RecordingStoreError> {
        let sidecar = sidecar_name(name);
        if sidecar.len() > MAX_RECORDING_FILENAME_BYTES {
            return Ok(());
        }
        let shared = self.shared()?;
        let descriptor = rustix_fs::openat(
            &shared.root,
            sidecar,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|source| RecordingStoreError::SidecarWrite(source.into()))?;
        File::from(descriptor)
            .write_all(meta.encode().as_bytes())
            .map_err(RecordingStoreError::SidecarWrite)
    }

    /// The newest partial recording of `stream` found when the root was first used by a recorder.
    pub(crate) fn startup_partial(&self, stream: &str) -> Option<String> {
        let shared = self.shared().ok()?;
        shared
            .startup_partials
            .get_or_init(|| {
                self.inventory()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|entry| entry.status == RecordingStatus::Partial)
                    .collect()
            })
            .iter()
            .find(|entry| entry.stream.as_deref() == Some(stream))
            .map(|entry| entry.name.clone())
    }

    pub(crate) fn remove_sidecar(&self, name: &str) {
        if let Ok(shared) = self.shared() {
            let _ = rustix_fs::unlinkat(&shared.root, sidecar_name(name), AtFlags::empty());
        }
    }

    /// Lists every recording in the root, newest modification first.
    ///
    /// A recording is `recording` while a recorder of this process writes it, `complete` once its
    /// sidecar says so, and `partial` when a sidecar says it was still being written but no
    /// recorder owns it (a crash or a failed recorder). Files without a sidecar are `complete` when
    /// their FLV tail is intact and `partial` otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be enumerated.
    pub fn inventory(&self) -> Result<Vec<RecordingEntry>, RecordingStoreError> {
        let shared = self.shared()?;
        let active = shared.lock().active.clone();
        let mut directory = Dir::read_from(&shared.root)
            .map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let mut entries = Vec::new();
        for entry in &mut directory {
            let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
            let Ok(name) = std::str::from_utf8(entry.file_name().to_bytes()) else {
                continue;
            };
            if is_hidden_name(name) {
                continue;
            }
            let Ok(metadata) = rustix_fs::statat(&shared.root, name, AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if !FileType::from_raw_mode(metadata.st_mode).is_file() {
                continue;
            }
            let size_bytes = u64::try_from(metadata.st_size).unwrap_or(0);
            let meta = read_sidecar(&shared.root, name);
            let status = if active.contains(name) {
                RecordingStatus::Recording
            } else if let Some(meta) = &meta {
                if meta.complete {
                    RecordingStatus::Complete
                } else {
                    RecordingStatus::Partial
                }
            } else if Self::flv_tail_intact(&shared, name, size_bytes) {
                RecordingStatus::Complete
            } else {
                RecordingStatus::Partial
            };
            let meta = meta.unwrap_or_default();
            let modified = modified_unix_ms(&metadata);
            entries.push(RecordingEntry {
                id: recording_id(shared.id_scope, name),
                name: name.to_owned(),
                status,
                size_bytes,
                modified_unix_ms: modified,
                stream: meta.stream,
                started_unix_ms: meta.started_unix_ms,
                ended_unix_ms: meta
                    .ended_unix_ms
                    .or_else(|| (status != RecordingStatus::Recording).then_some(modified)),
                duration_ms: meta.duration_ms,
                video_codec: meta.video_codec,
                audio_codec: meta.audio_codec,
            });
        }
        entries.sort_by(|left, right| {
            (right.modified_unix_ms, &right.name).cmp(&(left.modified_unix_ms, &left.name))
        });
        Ok(entries)
    }

    fn flv_tail_intact(shared: &StoreShared, name: &str, length: u64) -> bool {
        rustix_fs::openat(
            &shared.root,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .is_ok_and(|descriptor| inspect_flv_tail(&mut File::from(descriptor), length).is_ok())
    }

    /// Opens one finished or partial recording for reading.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid name or when the file cannot be opened as a regular file.
    pub fn open_recording(&self, name: &str) -> Result<(File, u64), RecordingStoreError> {
        validate_relative_name(name)?;
        if is_hidden_name(name) {
            return Err(RecordingStoreError::InvalidRelativeName);
        }
        let shared = self.shared()?;
        let descriptor = rustix_fs::openat(
            &shared.root,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| RecordingStoreError::RecordingOpen(source.into()))?;
        let metadata = rustix_fs::fstat(&descriptor)
            .map_err(|source| RecordingStoreError::RecordingOpen(source.into()))?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(RecordingStoreError::InvalidRelativeName);
        }
        Ok((
            File::from(descriptor),
            u64::try_from(metadata.st_size).unwrap_or(0),
        ))
    }

    /// Deletes one recording and its sidecar. A recording a recorder is writing is refused.
    ///
    /// # Errors
    ///
    /// Returns an error when the recording is active, missing, or cannot be unlinked.
    pub fn delete_recording(&self, name: &str) -> Result<u64, RecordingStoreError> {
        validate_relative_name(name)?;
        if is_hidden_name(name) {
            return Err(RecordingStoreError::InvalidRelativeName);
        }
        let shared = self.shared()?;
        let mut state = shared.lock();
        if state.active.contains(name) {
            return Err(RecordingStoreError::RecordingActive);
        }
        let metadata = rustix_fs::statat(&shared.root, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| RecordingStoreError::RecordingDelete(source.into()))?;
        let bytes = u64::try_from(metadata.st_size).unwrap_or(0);
        shared.delete_finished(&mut state, name, bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
