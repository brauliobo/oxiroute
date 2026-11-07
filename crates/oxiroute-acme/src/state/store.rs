#[allow(clippy::wildcard_imports)]
use super::*;

pub struct StateStore {
    root: PathBuf,
    _lock: Arc<File>,
}

impl fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("root", &self.root)
            .field("lock", &"held")
            .finish_non_exhaustive()
    }
}

impl StateStore {
    /// Opens an owner-only state root and retains an exclusive process-lifetime lock.
    ///
    /// # Errors
    ///
    /// Returns an error when any path component, owner, mode, or lock is unsafe.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, AcmeStateError> {
        let root = root.as_ref();
        if !root.is_absolute() {
            return Err(AcmeStateError::StateRootNotAbsolute);
        }
        ensure_state_root(root)?;

        let lock_path = root.join(".lock");
        let lock = shared_lock(root, &lock_path)?;
        Ok(Self {
            root: root.into(),
            _lock: lock,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes secret bytes with owner-only permissions using a synced temporary file and rename.
    ///
    /// # Errors
    ///
    /// Returns an error when the path, bound, permissions, or atomic write cannot be satisfied.
    pub fn write_secret(&self, relative: &str, secret: &SecretBytes) -> Result<(), AcmeStateError> {
        self.write_atomic(relative, secret.as_bytes(), SECRET_MODE)
    }

    /// Writes non-secret state with bounded owner-readable permissions and atomic replacement.
    ///
    /// # Errors
    ///
    /// Returns an error when the path, bound, permissions, or atomic write cannot be satisfied.
    pub fn write_public(&self, relative: &str, bytes: &[u8]) -> Result<(), AcmeStateError> {
        self.write_atomic(relative, bytes, PUBLIC_MODE)
    }

    /// Serializes and atomically writes a bounded non-secret JSON state document.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization, the path, bound, permissions, or atomic write fails.
    pub fn write_json<T: Serialize>(
        &self,
        relative: &str,
        value: &T,
    ) -> Result<(), AcmeStateError> {
        let bytes = serde_json::to_vec(value).map_err(AcmeStateError::Json)?;
        self.write_atomic(relative, &bytes, PUBLIC_MODE)
    }

    /// Serializes and atomically writes a bounded secret JSON state document with owner-only mode.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization, the path, bound, permissions, or atomic write fails.
    pub fn write_secret_json<T: Serialize>(
        &self,
        relative: &str,
        value: &T,
    ) -> Result<(), AcmeStateError> {
        let bytes = serde_json::to_vec(value).map_err(AcmeStateError::Json)?;
        self.write_atomic(relative, &bytes, SECRET_MODE)
    }

    /// Reads a regular no-follow file twice and accepts it only when both bounded reads match.
    ///
    /// # Errors
    ///
    /// Returns an error when the path, file type, bound, read, or stability check fails.
    pub fn read_bounded(&self, relative: &str, limit: usize) -> Result<Vec<u8>, AcmeStateError> {
        let path = self.safe_path(relative)?;
        let first = read_once(&path, limit)?;
        let second = read_once(&path, limit)?;
        if first != second {
            return Err(AcmeStateError::FileChanged);
        }
        Ok(first)
    }

    /// Reads and deserializes a bounded stable JSON state document.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read stably or contains invalid JSON.
    pub fn read_json<T: for<'de> Deserialize<'de>>(
        &self,
        relative: &str,
        limit: usize,
    ) -> Result<T, AcmeStateError> {
        let bytes = self.read_bounded(relative, limit)?;
        serde_json::from_slice(&bytes).map_err(AcmeStateError::Json)
    }

    /// Reads and deserializes a bounded stable secret JSON state document.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read stably or contains invalid JSON.
    pub fn read_secret_json<T: for<'de> Deserialize<'de>>(
        &self,
        relative: &str,
        limit: usize,
    ) -> Result<T, AcmeStateError> {
        self.read_json(relative, limit)
    }

    /// Removes one managed state file and synchronizes its parent directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is unsafe, the file cannot be removed, or the parent cannot be
    /// synchronized.
    pub fn remove_file(&self, relative: &str) -> Result<(), AcmeStateError> {
        let path = self.safe_path(relative)?;
        let parent = path.parent().ok_or(AcmeStateError::UnsafePath)?;
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(parent),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AcmeStateError::FileWrite(error)),
        }
    }

    /// Creates one queued lifecycle job under the bounded jobs namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is unsafe, the job already exists, or persistence fails.
    pub fn create_job(
        &self,
        id: impl Into<String>,
        certificate: impl Into<String>,
        operation: impl Into<String>,
        correlation_id: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<JobState, AcmeStateError> {
        let job = JobState {
            id: id.into(),
            certificate: certificate.into(),
            operation: operation.into(),
            correlation_id,
            status: JobStatus::Queued,
            created_at_unix_seconds: now_unix_seconds,
            updated_at_unix_seconds: now_unix_seconds,
            attempt: 0,
            next_action_unix_seconds: None,
            disk_revision: None,
            active_revision: None,
            last_outcome: None,
        };
        match self.read_job(&job.id) {
            Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(AcmeStateError::InvalidJobTransition),
            Err(error) => return Err(error),
        }
        self.persist_job(&job)?;
        Ok(job)
    }

    /// Moves a queued job to its first bounded execution attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the persisted job cannot legally start or cannot be written.
    pub fn start_job(&self, id: &str, now_unix_seconds: u64) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Running,
            now_unix_seconds,
            None,
            None,
            None,
            None,
        )
    }

    /// Marks a running issuance job as waiting for challenge completion.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn wait_job(&self, id: &str, now_unix_seconds: u64) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::WaitingForChallenge,
            now_unix_seconds,
            None,
            None,
            None,
            None,
        )
    }

    /// Marks a running or challenge-complete issuance job as finalizing.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn finalize_job(
        &self,
        id: &str,
        now_unix_seconds: u64,
    ) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Finalizing,
            now_unix_seconds,
            None,
            None,
            None,
            None,
        )
    }

    /// Completes an active job successfully with a redacted outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn succeed_job(
        &self,
        id: &str,
        now_unix_seconds: u64,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        outcome: RedactedOutcome,
    ) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Succeeded,
            now_unix_seconds,
            None,
            disk_revision,
            active_revision,
            Some(outcome),
        )
    }

    /// Completes an active job unsuccessfully with an optional retry time.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn fail_job(
        &self,
        id: &str,
        now_unix_seconds: u64,
        next_action_unix_seconds: Option<u64>,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        outcome: RedactedOutcome,
    ) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Failed,
            now_unix_seconds,
            next_action_unix_seconds,
            disk_revision,
            active_revision,
            Some(outcome),
        )
    }

    /// Completes an active job as cooperatively cancelled.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn cancel_job(
        &self,
        id: &str,
        now_unix_seconds: u64,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        outcome: RedactedOutcome,
    ) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Cancelled,
            now_unix_seconds,
            None,
            disk_revision,
            active_revision,
            Some(outcome),
        )
    }

    /// Completes an active job because managed issuance was paused.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition or emitted job state is invalid.
    pub fn pause_job(
        &self,
        id: &str,
        now_unix_seconds: u64,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        outcome: RedactedOutcome,
    ) -> Result<JobState, AcmeStateError> {
        self.transition_job(
            id,
            JobStatus::Paused,
            now_unix_seconds,
            None,
            disk_revision,
            active_revision,
            Some(outcome),
        )
    }

    /// Loads one redacted lifecycle job document from the jobs namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the job identity is unsafe, missing, oversized, or malformed.
    pub fn read_job(&self, id: &str) -> Result<JobState, AcmeStateError> {
        validate_slug(id)?;
        self.read_json(&format!("jobs/{id}.json"), MAX_JOB_BYTES)
    }

    #[allow(clippy::too_many_arguments)]
    fn transition_job(
        &self,
        id: &str,
        status: JobStatus,
        now_unix_seconds: u64,
        next_action_unix_seconds: Option<u64>,
        disk_revision: Option<String>,
        active_revision: Option<String>,
        last_outcome: Option<RedactedOutcome>,
    ) -> Result<JobState, AcmeStateError> {
        let current = self.read_job(id)?;
        if now_unix_seconds < current.updated_at_unix_seconds
            || !legal_job_transition(&current.status, &status)
        {
            return Err(AcmeStateError::InvalidJobTransition);
        }
        let attempt = if status == JobStatus::Running {
            current
                .attempt
                .checked_add(1)
                .filter(|attempt| *attempt <= MAX_JOB_ATTEMPTS)
                .ok_or(AcmeStateError::InvalidJobTransition)?
        } else {
            current.attempt
        };
        let job = JobState {
            id: current.id,
            certificate: current.certificate,
            operation: current.operation,
            correlation_id: current.correlation_id,
            status,
            created_at_unix_seconds: current.created_at_unix_seconds,
            updated_at_unix_seconds: now_unix_seconds,
            attempt,
            next_action_unix_seconds,
            disk_revision,
            active_revision,
            last_outcome,
        };
        self.persist_job(&job)?;
        Ok(job)
    }

    fn persist_job(&self, job: &JobState) -> Result<(), AcmeStateError> {
        validate_job_identity(job)?;
        if !is_valid_emitted_job(job) {
            return Err(AcmeStateError::InvalidJob);
        }
        self.write_json(&format!("jobs/{}.json", job.id), job)
    }

    fn write_atomic(&self, relative: &str, bytes: &[u8], mode: u32) -> Result<(), AcmeStateError> {
        if bytes.len() > MAX_STATE_FILE_BYTES {
            return Err(AcmeStateError::FileTooLarge {
                limit: MAX_STATE_FILE_BYTES,
            });
        }
        let path = self.safe_path(relative)?;
        let parent = path.parent().ok_or(AcmeStateError::UnsafePath)?;
        self.ensure_managed_directory(parent)?;

        let mut temporary = None;
        for _ in 0..MAX_TEMP_ATTEMPTS {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".oxiroute-acme.{sequence}.tmp"));
            match open_private_temp(&candidate) {
                Ok(file) => {
                    temporary = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(AcmeStateError::FileOpen(error)),
            }
        }
        let Some((temporary, mut file)) = temporary else {
            return Err(AcmeStateError::FileOpen(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary-file bound exhausted",
            )));
        };

        if let Err(error) = write_and_sync(&mut file, bytes, mode) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, &path).map_err(AcmeStateError::Rename)?;
        sync_directory(parent)
    }

    pub(super) fn safe_path(&self, relative: &str) -> Result<PathBuf, AcmeStateError> {
        let relative = Path::new(relative);
        if relative.as_os_str().len() > 4_096 || relative.is_absolute() {
            return Err(AcmeStateError::UnsafePath);
        }
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(AcmeStateError::UnsafePath);
            };
            let bytes = component.to_string_lossy();
            if bytes.is_empty() || bytes.len() > MAX_SLUG_BYTES || !is_safe_component(&bytes) {
                return Err(AcmeStateError::UnsafePath);
            }
        }
        if relative.components().next().is_none() {
            return Err(AcmeStateError::UnsafePath);
        }
        Ok(self.root.join(relative))
    }

    pub(super) fn ensure_managed_directory(&self, path: &Path) -> Result<(), AcmeStateError> {
        if !path.starts_with(&self.root) {
            return Err(AcmeStateError::UnsafePath);
        }
        ensure_managed_directory(&self.root, path)
    }
}
