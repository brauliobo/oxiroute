#[allow(clippy::wildcard_imports)]
use super::*;

/// One active recorder slot retained for a worker's full lifetime.
pub struct RecorderLease {
    pub(super) shared: Arc<StoreShared>,
}

/// Exclusive writable ownership of one recording path.
pub struct RecordingFile {
    pub(super) incarnation: u64,
    pub(super) shared: Arc<StoreShared>,
    pub(super) _ownership: OwnedFd,
    pub(super) file: Option<File>,
    pub(super) partial_name: String,
    pub(super) final_relative_name: String,
    pub(super) position: u64,
    pub(super) length: u64,
    pub(super) partial_exists: bool,
    pub(super) partial_exists_state: Arc<AtomicBool>,
    pub(super) file_accounted: bool,
    pub(super) max_bytes: Option<u64>,
    pub(super) preserve_partial: Arc<AtomicBool>,
    pub(super) commit: Arc<RecordingCommitState>,
    pub(super) resumed: bool,
}

impl RecordingFile {
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.length
    }

    pub(crate) fn partial_relative_name(&self) -> &str {
        &self.partial_name
    }

    pub(crate) fn preservation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.preserve_partial)
    }

    pub(crate) fn partial_existence_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.partial_exists_state)
    }

    pub(crate) fn commit_cancellation(&self) -> RecordingCommitCancellation {
        RecordingCommitCancellation {
            state: Arc::clone(&self.commit),
        }
    }

    pub(crate) fn finalizer_authority(&self) -> RecordingFinalizerAuthority {
        RecordingFinalizerAuthority {
            incarnation: self.incarnation,
            shared: Arc::clone(&self.shared),
        }
    }

    pub(crate) fn set_final_relative_name(
        &mut self,
        final_relative_name: String,
    ) -> Result<(), RecordingStoreError> {
        validate_relative_name(&final_relative_name)?;
        self.final_relative_name = final_relative_name;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn fail_publication_unlinks(&self) {
        self.commit
            .fail_partial_unlink
            .store(true, Ordering::Release);
        self.commit
            .fail_rollback_unlink
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn fail_partial_unlink(&self) {
        self.commit
            .fail_partial_unlink
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn fail_partial_unlink_and_rollback_sync(&self) {
        self.fail_partial_unlink();
        self.commit
            .fail_rollback_sync
            .store(true, Ordering::Release);
    }

    /// Flushes and synchronizes the recording, then publishes any renamed target without replacing
    /// an existing entry. A collision receives a bounded deterministic relative suffix.
    ///
    /// # Errors
    ///
    /// Returns an error when flushing, synchronizing, publishing, or synchronizing the containing
    /// directory fails. A directory-sync error carries the already-published recording.
    pub fn commit(mut self) -> Result<RecordingCommit, RecordingStoreError> {
        self.commit_inner()
    }

    fn commit_inner(&mut self) -> Result<RecordingCommit, RecordingStoreError> {
        self.preserve_partial.store(true, Ordering::Release);
        if self.commit.cancelled() {
            return Err(self.cancelled_error());
        }
        let operation = self
            .shared
            .claim(self.incarnation)
            .ok_or_else(|| self.cancelled_error())?;
        if let Some(file) = self.file.as_mut()
            && let Err(source) = file.flush().and_then(|()| file.sync_all())
        {
            self.commit.finish();
            return Err(RecordingStoreError::FileSync {
                partial_relative_name: self.partial_name.clone(),
                source,
            });
        }
        if self.commit.cancelled() {
            return Err(self.cancelled_error());
        }
        if !operation.incarnation_active() {
            return Err(self.cancelled_error());
        }
        if !self.partial_is_owned() {
            self.partial_exists = false;
            self.partial_exists_state.store(false, Ordering::Release);
            self.release_missing_file_accounting();
            self.commit.finish();
            return Err(RecordingStoreError::PartialOwnershipLost {
                partial_relative_name: self.partial_name.clone(),
            });
        }
        drop(operation);
        #[cfg(test)]
        self.commit.wait_before_publication();
        // Lock order: establish the store-state claim before changing the commit atomic. No path
        // may hold the commit gate/finalizer mutex while acquiring the store-state mutex.
        let _publication = self
            .shared
            .claim(self.incarnation)
            .ok_or_else(|| self.cancelled_error())?;
        if !self.commit.begin_publication() {
            return Err(self.cancelled_error());
        }
        #[cfg(test)]
        self.commit.wait_after_publication_claim();
        let relative_name = if self.partial_name == self.final_relative_name {
            self.final_relative_name.clone()
        } else {
            match self.publish() {
                Ok(relative_name) => relative_name,
                Err(error) => {
                    self.commit.finish();
                    return Err(error);
                }
            }
        };
        self.partial_exists = false;
        self.partial_exists_state.store(false, Ordering::Release);
        self.file_accounted = false;
        let recording = RecordingCommit {
            relative_name,
            bytes: self.length,
        };
        if let Err(source) = rustix_fs::fsync(&self.shared.root) {
            self.commit.finish();
            return Err(RecordingStoreError::PublishedDirectorySync {
                recording,
                source: source.into(),
            });
        }
        self.commit.finish();
        Ok(recording)
    }

    fn cancelled_error(&self) -> RecordingStoreError {
        RecordingStoreError::FinalizationCancelled {
            partial_relative_name: self.partial_name.clone(),
        }
    }

    fn partial_is_owned(&self) -> bool {
        let Some(file) = self.file.as_ref() else {
            return false;
        };
        let Ok(descriptor) = rustix_fs::fstat(file) else {
            return false;
        };
        let Ok(path) = rustix_fs::statat(
            &self.shared.root,
            self.partial_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            return false;
        };
        FileType::from_raw_mode(path.st_mode).is_file()
            && descriptor.st_dev == path.st_dev
            && descriptor.st_ino == path.st_ino
    }

    fn publish(&mut self) -> Result<String, RecordingStoreError> {
        for attempt in 0..MAX_NAME_ATTEMPTS {
            let candidate = if attempt == 0 {
                self.final_relative_name.clone()
            } else {
                let Some(candidate) =
                    collision_recording_filename(&self.final_relative_name, attempt)
                else {
                    break;
                };
                candidate
            };
            let file = self
                .file
                .as_ref()
                .expect("recording descriptor remains open through publication");
            match rustix_fs::linkat(
                file,
                "",
                &self.shared.root,
                candidate.as_str(),
                AtFlags::EMPTY_PATH,
            ) {
                Ok(()) => {
                    if self.partial_is_owned() {
                        let unlink_partial = if publication_partial_unlink_fault(&self.commit) {
                            Err(Errno::IO)
                        } else {
                            rustix_fs::unlinkat(
                                &self.shared.root,
                                self.partial_name.as_str(),
                                AtFlags::empty(),
                            )
                        };
                        if let Err(source) = unlink_partial {
                            return Err(self.rollback_publication(candidate, source));
                        }
                    }
                    return Ok(candidate);
                }
                Err(Errno::EXIST) => {}
                Err(source)
                    if source == Errno::PERM
                        || source == Errno::INVAL
                        || source == Errno::NOTSUP =>
                {
                    return Err(RecordingStoreError::DescriptorPublicationUnsupported {
                        partial_relative_name: self.partial_name.clone(),
                    });
                }
                Err(source) => {
                    if !self.partial_is_owned() {
                        self.partial_exists = false;
                        self.partial_exists_state.store(false, Ordering::Release);
                        self.release_missing_file_accounting();
                        return Err(RecordingStoreError::PartialOwnershipLost {
                            partial_relative_name: self.partial_name.clone(),
                        });
                    }
                    return Err(RecordingStoreError::Publish {
                        partial_relative_name: self.partial_name.clone(),
                        source: source.into(),
                    });
                }
            }
        }
        Err(RecordingStoreError::FinalNameCollisions {
            partial_relative_name: self.partial_name.clone(),
        })
    }

    fn rollback_publication(&mut self, candidate: String, source: Errno) -> RecordingStoreError {
        let recording = RecordingCommit {
            relative_name: candidate,
            bytes: self.length,
        };
        let rollback = if publication_rollback_unlink_fault(&self.commit) {
            Err(Errno::IO)
        } else {
            rustix_fs::unlinkat(
                &self.shared.root,
                recording.relative_name.as_str(),
                AtFlags::empty(),
            )
        };
        if let Err(rollback_source) = rollback {
            self.account_duplicate_publication();
            return RecordingStoreError::PublishRollback {
                partial_relative_name: self.partial_name.clone(),
                recording,
                source: source.into(),
                rollback_source: rollback_source.into(),
            };
        }
        let rollback_sync = if publication_rollback_sync_fault(&self.commit) {
            Err(Errno::IO)
        } else {
            rustix_fs::fsync(&self.shared.root)
        };
        if let Err(directory_sync_source) = rollback_sync {
            return RecordingStoreError::PublishRollbackDirectorySync {
                partial_relative_name: self.partial_name.clone(),
                recording,
                source: source.into(),
                directory_sync_source: directory_sync_source.into(),
            };
        }
        RecordingStoreError::Publish {
            partial_relative_name: self.partial_name.clone(),
            source: source.into(),
        }
    }

    fn account_duplicate_publication(&mut self) {
        let mut state = self.shared.lock();
        state.files = state.files.saturating_add(1);
        state.bytes_used = state.bytes_used.saturating_add(self.length);
    }

    fn release_missing_file_accounting(&mut self) {
        if !self.file_accounted {
            return;
        }
        let mut state = self.shared.lock();
        state.files -= 1;
        state.bytes_used -= self.length;
        self.file_accounted = false;
    }
}

impl Write for RecordingFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        let requested = u64::try_from(buffer.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording write is too large")
        })?;
        let requested_end = self.position.checked_add(requested).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "recording position overflow")
        })?;
        if self
            .max_bytes
            .is_some_and(|maximum| requested_end > maximum)
        {
            return Err(byte_quota_error(
                self.max_bytes.expect("recording byte limit was checked"),
            ));
        }
        let reserved_growth = requested_end.saturating_sub(self.length);
        {
            let mut state = self.shared.lock();
            let Some(new_total) = state.bytes_used.checked_add(reserved_growth) else {
                return Err(byte_quota_error(
                    self.shared.limits.max_bytes.unwrap_or(u64::MAX),
                ));
            };
            if let Some(maximum) = self.shared.limits.max_bytes
                && new_total > maximum
            {
                return Err(byte_quota_error(maximum));
            }
            state.bytes_used = new_total;
        }

        let result = self
            .file
            .as_mut()
            .expect("recording file is writable until commit")
            .write(buffer);
        match result {
            Ok(written) => {
                let written = u64::try_from(written).expect("written byte count fits in u64");
                let actual_end = self.position + written;
                let actual_growth = actual_end.saturating_sub(self.length);
                if actual_growth < reserved_growth {
                    self.shared.lock().bytes_used -= reserved_growth - actual_growth;
                }
                self.position = actual_end;
                self.length = self.length.max(actual_end);
                Ok(usize::try_from(written).expect("write returned a usize byte count"))
            }
            Err(source) => {
                self.shared.lock().bytes_used -= reserved_growth;
                Err(source)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        self.file
            .as_mut()
            .expect("recording file is writable until commit")
            .flush()
    }
}

impl Seek for RecordingFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let _claim = self.shared.claim(self.incarnation).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "recording store incarnation retired",
            )
        })?;
        let position = self
            .file
            .as_mut()
            .expect("recording file is seekable until commit")
            .seek(position)?;
        self.position = position;
        Ok(position)
    }
}

impl Drop for RecordingFile {
    fn drop(&mut self) {
        if !self.file_accounted && !self.partial_exists {
            return;
        }
        let Some(_claim) = self.shared.claim(self.incarnation) else {
            return;
        };
        if self.resumed {
            let owned = self.partial_is_owned();
            drop(self.file.take());
            if !owned {
                self.partial_exists_state.store(false, Ordering::Release);
                self.release_missing_file_accounting();
            }
            return;
        }

        let owned = self.partial_is_owned();
        drop(self.file.take());
        let preserve_partial = self.preserve_partial.load(Ordering::Acquire);
        let removed = if self.partial_exists && !preserve_partial && owned {
            match rustix_fs::unlinkat(
                &self.shared.root,
                self.partial_name.as_str(),
                AtFlags::empty(),
            ) {
                Ok(()) | Err(Errno::NOENT) => true,
                Err(_) => false,
            }
        } else {
            !(self.partial_exists && preserve_partial && owned)
        };
        let mut state = self.shared.lock();
        if removed && self.file_accounted {
            state.files -= 1;
            state.bytes_used -= self.length;
            self.file_accounted = false;
        }
        if removed || !owned {
            self.partial_exists_state.store(false, Ordering::Release);
        }
    }
}

impl Drop for RecorderLease {
    fn drop(&mut self) {
        self.shared.lock().active_recorders -= 1;
    }
}
