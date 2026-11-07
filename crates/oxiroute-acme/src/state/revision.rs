#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub struct RevisionStore {
    state: Arc<StateStore>,
}

impl RevisionStore {
    #[must_use]
    pub fn new(state: StateStore) -> Self {
        Self {
            state: Arc::new(state),
        }
    }

    #[must_use]
    pub fn from_arc(state: Arc<StateStore>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn state(&self) -> &StateStore {
        &self.state
    }

    /// Commits a complete certificate revision before atomically advancing its current pointer.
    ///
    /// # Errors
    ///
    /// Returns an error when names, bounds, material, permissions, synchronization, or publication
    /// fail. Existing revisions are not removed on failure.
    pub fn commit(
        &self,
        certificate: &str,
        revision: &str,
        material: &CertificateMaterial,
    ) -> Result<(), AcmeStateError> {
        validate_slug(certificate)?;
        validate_revision(revision)?;
        if material.certificate_pem.is_empty()
            || material.fullchain_pem.is_empty()
            || material.private_key_pem.as_bytes().is_empty()
        {
            return Err(AcmeStateError::IncompleteCertificate);
        }
        if material.certificate_pem.len() > MAX_CERTIFICATE_BYTES
            || material.chain_pem.len() > MAX_CERTIFICATE_BYTES
            || material.fullchain_pem.len() > MAX_CERTIFICATE_BYTES
            || material.private_key_pem.as_bytes().len() > MAX_CERTIFICATE_BYTES
        {
            return Err(AcmeStateError::FileTooLarge {
                limit: MAX_CERTIFICATE_BYTES,
            });
        }

        let base = format!("certificates/{certificate}/revisions/{revision}");
        self.state
            .write_public(&format!("{base}/cert.pem"), &material.certificate_pem)?;
        self.state
            .write_public(&format!("{base}/chain.pem"), &material.chain_pem)?;
        self.state
            .write_public(&format!("{base}/fullchain.pem"), &material.fullchain_pem)?;
        self.state
            .write_secret(&format!("{base}/privkey.pem"), &material.private_key_pem)?;
        self.state
            .write_json(&format!("{base}/metadata.json"), &material.metadata)?;
        install_current(&self.state, certificate, revision)?;
        Ok(())
    }

    /// Loads the complete material selected by the current relative revision pointer.
    ///
    /// # Errors
    ///
    /// Returns an error when the pointer escapes the certificate directory or any material cannot
    /// be read stably within its bound.
    pub fn load_current(&self, certificate: &str) -> Result<CertificateMaterial, AcmeStateError> {
        validate_slug(certificate)?;
        let current = format!("certificates/{certificate}/current");
        let target =
            fs::read_link(self.state.safe_path(&current)?).map_err(AcmeStateError::FileOpen)?;
        if target.is_absolute()
            || target
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AcmeStateError::UnsafePath);
        }
        let base = format!("certificates/{certificate}/{}/", target.to_string_lossy());
        let metadata = self
            .state
            .read_json::<RevisionMetadata>(&format!("{base}metadata.json"), MAX_JOB_BYTES)?;
        let private_key = SecretBytes::new(
            self.state
                .read_bounded(&format!("{base}privkey.pem"), MAX_CERTIFICATE_BYTES)?,
        );
        Ok(CertificateMaterial {
            certificate_pem: self
                .state
                .read_bounded(&format!("{base}cert.pem"), MAX_CERTIFICATE_BYTES)?,
            chain_pem: self
                .state
                .read_bounded(&format!("{base}chain.pem"), MAX_CERTIFICATE_BYTES)?,
            fullchain_pem: self
                .state
                .read_bounded(&format!("{base}fullchain.pem"), MAX_CERTIFICATE_BYTES)?,
            private_key_pem: private_key,
            metadata,
        })
    }

    /// Removes old complete revisions while always preserving the current pointer and the newest
    /// configured revisions.
    ///
    /// # Errors
    ///
    /// Returns an error when the certificate namespace, current pointer, revision metadata, or
    /// deletion path is unsafe or unavailable.
    pub fn garbage_collect(
        &self,
        certificate: &str,
        retain_revisions: usize,
        retain_after_unix_seconds: u64,
    ) -> Result<usize, AcmeStateError> {
        validate_slug(certificate)?;
        if retain_revisions == 0 {
            return Err(AcmeStateError::InvalidRevision);
        }
        let revisions_relative = format!("certificates/{certificate}/revisions");
        let revisions_path = self.state.safe_path(&revisions_relative)?;
        let current = current_revision(&self.state, certificate)?;
        let entries = match fs::read_dir(&revisions_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(AcmeStateError::FileOpen(error)),
        };
        let mut revisions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AcmeStateError::FileOpen)?;
            let revision = entry
                .file_name()
                .to_str()
                .ok_or(AcmeStateError::UnsafePath)?
                .to_owned();
            validate_revision(&revision)?;
            let metadata = entry.file_type().map_err(AcmeStateError::FileOpen)?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err(AcmeStateError::UnsafeDirectory);
            }
            let relative = format!("{revisions_relative}/{revision}/metadata.json");
            let metadata = self
                .state
                .read_json::<RevisionMetadata>(&relative, MAX_JOB_BYTES)?;
            if metadata.certificate != certificate || metadata.revision != revision {
                return Err(AcmeStateError::InvalidRevision);
            }
            revisions.push((revision, metadata.created_at_unix_seconds));
        }
        revisions.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
        let mut protected = BTreeSet::new();
        if let Some(current) = current {
            protected.insert(current);
        }
        protected.extend(
            revisions
                .iter()
                .take(retain_revisions)
                .map(|(revision, _)| revision.clone()),
        );
        let mut removed = 0;
        for (revision, created_at) in revisions {
            if protected.contains(&revision) || created_at >= retain_after_unix_seconds {
                continue;
            }
            remove_revision(&self.state, certificate, &revision)?;
            removed += 1;
        }
        if removed > 0 {
            sync_directory(&revisions_path)?;
        }
        Ok(removed)
    }

    /// Deletes all persisted account and certificate state for one managed certificate.
    ///
    /// The active in-memory generation is owned by the server and is intentionally outside this
    /// operation. A caller must ensure that the certificate is no longer referenced before calling
    /// this method.
    ///
    /// # Errors
    ///
    /// Returns an error when any namespace, entry, or deletion path is unsafe or unavailable.
    pub fn delete_certificate_state(&self, certificate: &str) -> Result<(), AcmeStateError> {
        validate_slug(certificate)?;
        let certificate_relative = format!("certificates/{certificate}");
        let certificate_path = self.state.safe_path(&certificate_relative)?;
        let certificate_exists = match fs::symlink_metadata(&certificate_path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(AcmeStateError::UnsafeDirectory);
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(AcmeStateError::FileOpen(error)),
        };
        if certificate_exists {
            match fs::symlink_metadata(certificate_path.join("dns-cleanup.json")) {
                Ok(_) => return Err(AcmeStateError::PendingDnsCleanup),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(AcmeStateError::FileOpen(error)),
            }
            let entries = fs::read_dir(&certificate_path).map_err(AcmeStateError::FileOpen)?;
            for entry in entries {
                let entry = entry.map_err(AcmeStateError::FileOpen)?;
                let name = entry
                    .file_name()
                    .to_str()
                    .ok_or(AcmeStateError::UnsafePath)?
                    .to_owned();
                match name.as_str() {
                    "revisions" => {
                        let revisions_path = entry.path();
                        let metadata = entry.file_type().map_err(AcmeStateError::FileOpen)?;
                        if !metadata.is_dir() || metadata.is_symlink() {
                            return Err(AcmeStateError::UnsafeDirectory);
                        }
                        let revisions =
                            fs::read_dir(&revisions_path).map_err(AcmeStateError::FileOpen)?;
                        for revision in revisions {
                            let revision = revision.map_err(AcmeStateError::FileOpen)?;
                            let revision_name = revision
                                .file_name()
                                .to_str()
                                .ok_or(AcmeStateError::UnsafePath)?
                                .to_owned();
                            validate_revision(&revision_name)?;
                            if !revision
                                .file_type()
                                .map_err(AcmeStateError::FileOpen)?
                                .is_dir()
                            {
                                return Err(AcmeStateError::UnsafeDirectory);
                            }
                            remove_revision(&self.state, certificate, &revision_name)?;
                        }
                        fs::remove_dir(revisions_path).map_err(AcmeStateError::FileWrite)?;
                    }
                    "current" | "renewal.json" => {
                        remove_file_if_present(&entry.path())?;
                    }
                    "dns-cleanup.json" => return Err(AcmeStateError::PendingDnsCleanup),
                    _ => return Err(AcmeStateError::UnsafePath),
                }
            }
            fs::remove_dir(&certificate_path).map_err(AcmeStateError::FileWrite)?;
            if let Some(parent) = certificate_path.parent() {
                sync_directory(parent)?;
            }
        }

        let account_relative = format!("accounts/{certificate}");
        let account_path = self.state.safe_path(&account_relative)?;
        let account_exists = match fs::symlink_metadata(&account_path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(AcmeStateError::UnsafeDirectory);
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(AcmeStateError::FileOpen(error)),
        };
        if account_exists {
            let entries = fs::read_dir(&account_path).map_err(AcmeStateError::FileOpen)?;
            for entry in entries {
                let entry = entry.map_err(AcmeStateError::FileOpen)?;
                let name = entry
                    .file_name()
                    .to_str()
                    .ok_or(AcmeStateError::UnsafePath)?
                    .to_owned();
                if !matches!(name.as_str(), "account-key.pem" | "account.json") {
                    return Err(AcmeStateError::UnsafePath);
                }
                remove_file_if_present(&entry.path())?;
            }
            fs::remove_dir(&account_path).map_err(AcmeStateError::FileWrite)?;
            if let Some(parent) = account_path.parent() {
                sync_directory(parent)?;
            }
        }
        Ok(())
    }
}

fn current_revision(
    state: &StateStore,
    certificate: &str,
) -> Result<Option<String>, AcmeStateError> {
    let current = format!("certificates/{certificate}/current");
    let target = match fs::read_link(state.safe_path(&current)?) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AcmeStateError::FileOpen(error)),
    };
    let mut components = target.components();
    let Some(Component::Normal(parent)) = components.next() else {
        return Err(AcmeStateError::UnsafePath);
    };
    let Some(Component::Normal(revision)) = components.next() else {
        return Err(AcmeStateError::UnsafePath);
    };
    if parent != "revisions" || components.next().is_some() {
        return Err(AcmeStateError::UnsafePath);
    }
    let revision = revision.to_str().ok_or(AcmeStateError::UnsafePath)?;
    validate_revision(revision)?;
    Ok(Some(revision.to_owned()))
}

fn remove_revision(
    state: &StateStore,
    certificate: &str,
    revision: &str,
) -> Result<(), AcmeStateError> {
    validate_slug(certificate)?;
    validate_revision(revision)?;
    let relative = format!("certificates/{certificate}/revisions/{revision}");
    let path = state.safe_path(&relative)?;
    for name in [
        "cert.pem",
        "chain.pem",
        "fullchain.pem",
        "privkey.pem",
        "metadata.json",
    ] {
        let file = path.join(name);
        match fs::remove_file(file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AcmeStateError::FileWrite(error)),
        }
    }
    fs::remove_dir(&path).map_err(AcmeStateError::FileWrite)
}

fn remove_file_if_present(path: &Path) -> Result<(), AcmeStateError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AcmeStateError::FileWrite(error)),
    }
}

fn install_current(
    state: &StateStore,
    certificate: &str,
    revision: &str,
) -> Result<(), AcmeStateError> {
    let directory = state.safe_path(&format!("certificates/{certificate}"))?;
    state.ensure_managed_directory(&directory)?;
    let temporary = directory.join(format!(
        ".current.{}.tmp",
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let target = format!("revisions/{revision}");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &temporary).map_err(AcmeStateError::Rename)?;
    #[cfg(not(unix))]
    return Err(AcmeStateError::Rename(io::Error::new(
        io::ErrorKind::Unsupported,
        "managed current pointers require symbolic links",
    )));
    fs::rename(&temporary, directory.join("current")).map_err(AcmeStateError::Rename)?;
    sync_directory(&directory)
}

#[must_use]
pub fn revision_id(material: &CertificateMaterial) -> String {
    use std::fmt::Write as _;

    let mut digest = Sha256::new();
    digest.update(&material.certificate_pem);
    digest.update(&material.chain_pem);
    digest.update(&material.fullchain_pem);
    digest.update(material.private_key_pem.as_bytes());
    digest.update(serde_json::to_vec(&material.metadata).unwrap_or_default());
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}
