#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ActiveReservation {
    shared: Arc<StoreShared>,
    name: String,
    armed: bool,
}

impl ActiveReservation {
    pub(super) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ActiveReservation {
    fn drop(&mut self) {
        if self.armed {
            self.shared.lock().active.remove(&self.name);
        }
    }
}

pub(super) struct FinishedRecording {
    pub(super) name: String,
    pub(super) bytes: u64,
    pub(super) modified: (i128, i128),
}

impl StoreShared {
    pub(super) fn reserve_active(self: &Arc<Self>, name: &str) -> Option<ActiveReservation> {
        self.lock()
            .active
            .insert(name.to_owned())
            .then(|| ActiveReservation {
                shared: Arc::clone(self),
                name: name.to_owned(),
                armed: true,
            })
    }

    pub(super) fn finished_recordings(
        &self,
        state: &StoreState,
    ) -> Result<Vec<FinishedRecording>, RecordingStoreError> {
        let mut directory = Dir::read_from(&self.root)
            .map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let mut finished = Vec::new();
        for entry in &mut directory {
            let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
            let name = entry.file_name();
            let Ok(text) = std::str::from_utf8(name.to_bytes()) else {
                continue;
            };
            if is_hidden_name(text) || state.active.contains(text) {
                continue;
            }
            let metadata = match rustix_fs::statat(&self.root, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(Errno::NOENT) => continue,
                Err(source) => return Err(RecordingStoreError::RootEntryMetadata(source.into())),
            };
            if FileType::from_raw_mode(metadata.st_mode).is_file() {
                finished.push(FinishedRecording {
                    name: text.to_owned(),
                    bytes: u64::try_from(metadata.st_size).unwrap_or(u64::MAX),
                    modified: (
                        i128::from(metadata.st_mtime),
                        i128::from(metadata.st_mtime_nsec),
                    ),
                });
            }
        }
        finished
            .sort_by(|left, right| (left.modified, &left.name).cmp(&(right.modified, &right.name)));
        Ok(finished)
    }

    pub(super) fn resync(&self, state: &mut StoreState) -> Result<(), RecordingStoreError> {
        let counted = scan_root(&self.root, RecordingStoreLimits::unbounded(), false)?;
        state.files = counted.files;
        state.bytes_used = counted.bytes_used;
        Ok(())
    }

    pub(super) fn delete_finished(
        &self,
        state: &mut StoreState,
        recording: &FinishedRecording,
    ) -> Result<(), RecordingStoreError> {
        match rustix_fs::unlinkat(&self.root, recording.name.as_str(), AtFlags::empty()) {
            Ok(()) => {}
            Err(Errno::NOENT) => return Ok(()),
            Err(source) => return Err(RecordingStoreError::RecordingDelete(source.into())),
        }
        let _ = rustix_fs::unlinkat(&self.root, sidecar_name(&recording.name), AtFlags::empty());
        state.files = state.files.saturating_sub(1);
        state.bytes_used = state.bytes_used.saturating_sub(recording.bytes);
        Ok(())
    }

    fn evict(
        &self,
        state: &mut StoreState,
        recording: &FinishedRecording,
    ) -> Result<(), RecordingStoreError> {
        self.delete_finished(state, recording)?;
        state.evicted_files += 1;
        state.evicted_bytes = state.evicted_bytes.saturating_add(recording.bytes);
        Ok(())
    }

    pub(super) fn evict_until(
        &self,
        state: &mut StoreState,
        fits: impl Fn(&StoreState) -> bool,
    ) -> Result<bool, RecordingStoreError> {
        if fits(state) {
            return Ok(true);
        }
        self.resync(state)?;
        if fits(state) || !self.limits.retention.delete_oldest {
            return Ok(fits(state));
        }
        for recording in self.finished_recordings(state)? {
            self.evict(state, &recording)?;
            if fits(state) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn purge_expired(&self, state: &mut StoreState) -> Result<(), RecordingStoreError> {
        let Some(max_age) = self.limits.retention.max_age else {
            return Ok(());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i128::from(elapsed.as_secs()));
        let cutoff = now - i128::from(max_age.as_secs());
        for recording in self.finished_recordings(state)? {
            if recording.modified.0 > cutoff {
                break;
            }
            self.evict(state, &recording)?;
        }
        Ok(())
    }
}

impl RecordingStoreLimits {
    pub(super) const fn unbounded() -> Self {
        Self {
            max_bytes: None,
            max_files: None,
            max_active_recorders: 1,
            retention: RecordingRetention {
                delete_oldest: false,
                max_age: None,
            },
        }
    }
}

pub(super) fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

pub(super) fn sidecar_name(recording: &str) -> String {
    format!("{SIDECAR_PREFIX}{recording}{SIDECAR_SUFFIX}")
}

pub(super) fn is_sidecar(name: &[u8]) -> bool {
    name.starts_with(SIDECAR_PREFIX.as_bytes()) && name.ends_with(SIDECAR_SUFFIX.as_bytes())
}

#[cfg(test)]
mod tests;
