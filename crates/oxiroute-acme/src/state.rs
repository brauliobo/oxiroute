use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(not(unix))]
use std::fs::OpenOptions;

use rustix::fs::{self as rustix_fs, FileType, FlockOperation, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

pub const MAX_STATE_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CERTIFICATE_BYTES: usize = 1024 * 1024;
pub const MAX_JOB_BYTES: usize = 64 * 1024;

const MAX_SLUG_BYTES: usize = 128;
const ROOT_MODE: u32 = 0o700;
const SECRET_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
const MAX_TEMP_ATTEMPTS: usize = 128;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SHARED_LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<File>>>> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum AcmeStateError {
    #[error("ACME state root must be an absolute path")]
    StateRootNotAbsolute,
    #[error("ACME state path contains an unsafe symbolic link or non-directory")]
    UnsafeDirectory,
    #[error("ACME state directory ownership or mode is unsafe")]
    UnsafeDirectoryPermissions,
    #[error("ACME state root is already locked by another process")]
    StateLocked,
    #[error("ACME state lock could not be opened")]
    LockOpen(#[source] io::Error),
    #[error("ACME state lock could not be acquired")]
    LockAcquire,
    #[error("ACME state path is invalid")]
    UnsafePath,
    #[error("ACME state file exceeds the {limit}-byte bound")]
    FileTooLarge { limit: usize },
    #[error("ACME state file is not a regular no-follow file")]
    NotRegularFile,
    #[error("ACME state file could not be opened")]
    FileOpen(#[source] io::Error),
    #[error("ACME state file could not be read")]
    FileRead(#[source] io::Error),
    #[error("ACME state file changed during a stable read")]
    FileChanged,
    #[error("ACME state file could not be written")]
    FileWrite(#[source] io::Error),
    #[error("ACME state file could not be synchronized")]
    FileSync(#[source] io::Error),
    #[error("ACME state revision could not be installed atomically")]
    Rename(#[source] io::Error),
    #[error("ACME state directory could not be synchronized")]
    DirectorySync(#[source] io::Error),
    #[error("ACME state JSON is invalid")]
    Json(#[source] serde_json::Error),
    #[error("ACME state revision is invalid")]
    InvalidRevision,
    #[error("ACME certificate material is incomplete")]
    IncompleteCertificate,
}

/// Secret bytes are intentionally not serializable or printable.
#[derive(Clone, Default)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    #[must_use]
    pub fn into_bytes(self) -> Zeroizing<Vec<u8>> {
        self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(REDACTED)")
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    WaitingForChallenge,
    Finalizing,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedactedOutcome {
    pub code: String,
    pub message: String,
}

impl RedactedOutcome {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobState {
    pub id: String,
    pub certificate: String,
    pub operation: String,
    pub status: JobStatus,
    pub created_at_unix_seconds: u64,
    pub updated_at_unix_seconds: u64,
    pub attempt: u32,
    pub next_action_unix_seconds: Option<u64>,
    pub disk_revision: Option<String>,
    pub active_revision: Option<String>,
    pub last_outcome: Option<RedactedOutcome>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionMetadata {
    pub certificate: String,
    pub revision: String,
    pub created_at_unix_seconds: u64,
    pub not_before_unix_seconds: Option<u64>,
    pub not_after_unix_seconds: Option<u64>,
    pub issuer: Option<String>,
    pub serial_fingerprint: Option<String>,
    pub key_type: Option<String>,
}

pub struct CertificateMaterial {
    pub certificate_pem: Vec<u8>,
    pub chain_pem: Vec<u8>,
    pub fullchain_pem: Vec<u8>,
    pub private_key_pem: SecretBytes,
    pub metadata: RevisionMetadata,
}

impl fmt::Debug for CertificateMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertificateMaterial")
            .field("certificate_bytes", &self.certificate_pem.len())
            .field("chain_bytes", &self.chain_pem.len())
            .field("fullchain_bytes", &self.fullchain_pem.len())
            .field("private_key", &self.private_key_pem)
            .field("metadata", &self.metadata)
            .finish()
    }
}

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

    /// Persists one redacted lifecycle job document under the bounded jobs namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the job identity or serialized state is unsafe or cannot be committed
    /// atomically.
    pub fn write_job(&self, job: &JobState) -> Result<(), AcmeStateError> {
        validate_slug(&job.id)?;
        validate_slug(&job.certificate)?;
        if job.operation.is_empty()
            || job.operation.len() > MAX_SLUG_BYTES
            || !is_safe_component(&job.operation)
        {
            return Err(AcmeStateError::UnsafePath);
        }
        self.write_json(&format!("jobs/{}.json", job.id), job)
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

    fn safe_path(&self, relative: &str) -> Result<PathBuf, AcmeStateError> {
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

    fn ensure_managed_directory(&self, path: &Path) -> Result<(), AcmeStateError> {
        if !path.starts_with(&self.root) {
            return Err(AcmeStateError::UnsafePath);
        }
        ensure_managed_directory(&self.root, path)
    }
}

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

fn validate_slug(value: &str) -> Result<(), AcmeStateError> {
    if value.is_empty()
        || value.len() > MAX_SLUG_BYTES
        || value == "."
        || value == ".."
        || !is_safe_component(value)
    {
        return Err(AcmeStateError::InvalidRevision);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), AcmeStateError> {
    if value.is_empty()
        || value.len() > MAX_SLUG_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AcmeStateError::InvalidRevision);
    }
    Ok(())
}

fn is_safe_component(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn ensure_state_root(path: &Path) -> Result<(), AcmeStateError> {
    let mut current = PathBuf::from(Path::new("/"));
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(AcmeStateError::UnsafeDirectory);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|_| AcmeStateError::UnsafeDirectory)?;
                if current == path {
                    set_directory_mode(&current)?;
                }
            }
            Err(_) => return Err(AcmeStateError::UnsafeDirectory),
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| AcmeStateError::UnsafeDirectory)?;
    if !metadata.is_dir() {
        return Err(AcmeStateError::UnsafeDirectory);
    }
    validate_directory_metadata(&metadata)
}

fn ensure_managed_directory(root: &Path, path: &Path) -> Result<(), AcmeStateError> {
    let mut current = root.to_owned();
    for component in path
        .strip_prefix(root)
        .map_err(|_| AcmeStateError::UnsafePath)?
        .components()
    {
        let Component::Normal(component) = component else {
            return Err(AcmeStateError::UnsafePath);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(AcmeStateError::UnsafeDirectory);
                }
                validate_directory_metadata(&metadata)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|_| AcmeStateError::UnsafeDirectory)?;
                set_directory_mode(&current)?;
                let metadata =
                    fs::symlink_metadata(&current).map_err(|_| AcmeStateError::UnsafeDirectory)?;
                validate_directory_metadata(&metadata)?;
            }
            Err(_) => return Err(AcmeStateError::UnsafeDirectory),
        }
    }
    Ok(())
}

fn validate_directory_metadata(metadata: &fs::Metadata) -> Result<(), AcmeStateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let mode = metadata.permissions().mode() & 0o7777;
        let uid = rustix::process::getuid().as_raw();
        if metadata.uid() != uid || mode != ROOT_MODE {
            return Err(AcmeStateError::UnsafeDirectoryPermissions);
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

fn set_directory_mode(path: &Path) -> Result<(), AcmeStateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(ROOT_MODE))
            .map_err(|_| AcmeStateError::UnsafeDirectoryPermissions)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn open_lock(path: &Path) -> Result<File, AcmeStateError> {
    #[cfg(unix)]
    {
        let (descriptor, created) = match rustix_fs::open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(SECRET_MODE),
        ) {
            Ok(descriptor) => (descriptor, true),
            Err(rustix::io::Errno::EXIST) => (
                rustix_fs::open(
                    path,
                    OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| AcmeStateError::LockOpen(io::Error::from(error)))?,
                false,
            ),
            Err(error) => return Err(AcmeStateError::LockOpen(io::Error::from(error))),
        };
        if created {
            rustix_fs::fchmod(&descriptor, Mode::from_raw_mode(SECRET_MODE))
                .map_err(|_| AcmeStateError::UnsafeDirectoryPermissions)?;
        }
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AcmeStateError::LockAcquire)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file()
            || metadata.st_uid != rustix::process::getuid().as_raw()
            || metadata.st_mode & 0o7777 != SECRET_MODE
        {
            return Err(AcmeStateError::UnsafeDirectoryPermissions);
        }
        rustix_fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| AcmeStateError::StateLocked)?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(AcmeStateError::LockOpen)
    }
}

fn shared_lock(root: &Path, path: &Path) -> Result<Arc<File>, AcmeStateError> {
    let locks = SHARED_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(open_lock(path)?);
    locks.insert(root.to_owned(), Arc::downgrade(&lock));
    Ok(lock)
}

fn open_private_temp(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        let descriptor = rustix_fs::open(
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(SECRET_MODE),
        )
        .map_err(io::Error::from)?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create_new(true).write(true).open(path)
    }
}

fn write_and_sync(file: &mut File, bytes: &[u8], mode: u32) -> Result<(), AcmeStateError> {
    file.write_all(bytes).map_err(AcmeStateError::FileWrite)?;
    #[cfg(unix)]
    rustix_fs::fchmod(&mut *file, Mode::from_raw_mode(mode)).map_err(|_| {
        AcmeStateError::FileWrite(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "chmod failed",
        ))
    })?;
    file.sync_all().map_err(AcmeStateError::FileSync)
}

fn read_once(path: &Path, limit: usize) -> Result<Vec<u8>, AcmeStateError> {
    #[cfg(unix)]
    let file = {
        let descriptor = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                AcmeStateError::NotRegularFile
            } else {
                AcmeStateError::FileOpen(io::Error::from(error))
            }
        })?;
        let metadata = rustix_fs::fstat(&descriptor).map_err(|_| AcmeStateError::NotRegularFile)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(AcmeStateError::NotRegularFile);
        }
        File::from(descriptor)
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(AcmeStateError::FileOpen)?;

    let mut bytes = Vec::new();
    file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(AcmeStateError::FileRead)?;
    if bytes.len() > limit {
        return Err(AcmeStateError::FileTooLarge { limit });
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), AcmeStateError> {
    #[cfg(unix)]
    {
        let descriptor = rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| AcmeStateError::DirectorySync(io::Error::from(error)))?;
        rustix_fs::fsync(&descriptor)
            .map_err(|error| AcmeStateError::DirectorySync(io::Error::from(error)))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn material() -> CertificateMaterial {
        CertificateMaterial {
            certificate_pem: b"cert".to_vec(),
            chain_pem: b"chain".to_vec(),
            fullchain_pem: b"fullchain".to_vec(),
            private_key_pem: SecretBytes::new(b"private".to_vec()),
            metadata: RevisionMetadata {
                certificate: "edge".into(),
                revision: "0123abcd".into(),
                created_at_unix_seconds: 10,
                not_before_unix_seconds: None,
                not_after_unix_seconds: None,
                issuer: None,
                serial_fingerprint: None,
                key_type: Some("ecdsa_p256".into()),
            },
        }
    }

    #[test]
    fn state_root_lock_and_revision_are_secure_and_redacted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = StateStore::open(directory.path().join("state")).expect("state");
        let revision = RevisionStore::new(state);
        let material = material();
        revision
            .commit("edge", "0123abcd", &material)
            .expect("commit");

        let loaded = revision.load_current("edge").expect("load");
        assert_eq!(loaded.certificate_pem, material.certificate_pem);
        assert_eq!(loaded.private_key_pem.as_bytes(), b"private");
        let debug = format!("{loaded:?}");
        assert!(debug.contains("SecretBytes(REDACTED)"));
        assert!(!debug.contains("private\""));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let root_mode = fs::metadata(directory.path().join("state"))
                .expect("root metadata")
                .permissions()
                .mode()
                & 0o777;
            let key_mode = fs::metadata(
                directory
                    .path()
                    .join("state/certificates/edge/revisions/0123abcd/privkey.pem"),
            )
            .expect("key metadata")
            .permissions()
            .mode()
                & 0o777;
            assert_eq!(root_mode, 0o700);
            assert_eq!(key_mode, 0o600);
        }
    }

    #[test]
    fn same_process_state_root_reuses_the_lock_for_generation_reload() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = StateStore::open(directory.path().join("state")).expect("first state");
        let second = StateStore::open(directory.path().join("state")).expect("second state");
        assert_eq!(first.root(), second.root());
    }

    #[test]
    fn old_revision_survives_current_pointer_replacement() {
        let directory = tempfile::tempdir().expect("tempdir");
        let revision =
            RevisionStore::new(StateStore::open(directory.path().join("state")).expect("state"));
        let first = material();
        revision.commit("edge", "00000001", &first).expect("first");
        let mut second = material();
        second.certificate_pem = b"new-cert".to_vec();
        revision
            .commit("edge", "00000002", &second)
            .expect("second");

        assert_eq!(
            revision
                .load_current("edge")
                .expect("current")
                .certificate_pem,
            b"new-cert"
        );
        assert!(
            directory
                .path()
                .join("state/certificates/edge/revisions/00000001/cert.pem")
                .is_file()
        );
    }

    #[test]
    fn job_state_is_durable_and_contains_only_redacted_outcomes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = StateStore::open(directory.path().join("state")).expect("state");
        let job = JobState {
            id: "job-1".into(),
            certificate: "edge".into(),
            operation: "renew".into(),
            status: JobStatus::Failed,
            created_at_unix_seconds: 1,
            updated_at_unix_seconds: 2,
            attempt: 1,
            next_action_unix_seconds: Some(3),
            disk_revision: None,
            active_revision: Some("old".into()),
            last_outcome: Some(RedactedOutcome::new("transport_failed", "retry scheduled")),
        };
        state.write_job(&job).expect("write job");
        assert_eq!(state.read_job("job-1").expect("read job"), job);
        let json =
            fs::read_to_string(directory.path().join("state/jobs/job-1.json")).expect("job JSON");
        assert!(!json.contains("private"));
    }

    #[test]
    fn unsafe_paths_and_invalid_revisions_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = StateStore::open(directory.path().join("state")).expect("state");
        assert!(matches!(
            state.write_public("../escape", b"x"),
            Err(AcmeStateError::UnsafePath)
        ));
        assert!(matches!(
            RevisionStore::new(state).commit("edge", "not-hex", &material()),
            Err(AcmeStateError::InvalidRevision)
        ));
    }
}
