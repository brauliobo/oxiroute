use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Write},
    os::unix::ffi::OsStrExt as _,
    path::{Component, Path},
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::{
    fd::OwnedFd,
    fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags},
    io::Errno,
};

use super::{
    ConfigCoordinatorPathError, ConfigDiagnostic, ConfigDiagnosticStage, ConfigRevision,
    MAX_CANONICAL_CONFIG_BYTES, diagnostic,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_TEMP_ATTEMPTS: usize = 128;
const SECURE_FILE_MODE: Mode = Mode::from_raw_mode(0o600);
const SECURE_FILE_MODE_BITS: u32 = 0o600;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);

pub(super) struct CanonicalStorage {
    file_name: OsString,
    directories: DirectoryChain,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReplaceControl {
    #[cfg(test)]
    pub fail_before_exchange: bool,
    #[cfg(test)]
    pub fail_commit_sync: bool,
    #[cfg(test)]
    pub fail_cleanup_sync: bool,
}

pub(super) enum ReplaceResult {
    Saved { cleanup_degraded: bool },
    Conflict(ConfigRevision),
}

enum RollbackResult {
    Restored,
    Conflict(ConfigRevision),
}

enum PrepareResult {
    Conflict(ConfigRevision),
    Exchanged(PreparedExchange),
}

struct PreparedExchange {
    temp: TempEntry,
    _candidate_file: File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StorageFailure {
    DirectoryOpen,
    DirectoryChanged,
    Lock,
    FileOpen,
    NotRegular,
    TooLarge,
    Read,
    Unstable,
    TempCreate,
    TempWrite,
    FileSync,
    Rename,
    DirectorySync,
    Rollback,
}

impl StorageFailure {
    pub(super) const fn diagnostic(self) -> ConfigDiagnostic {
        match self {
            Self::DirectoryOpen | Self::FileOpen | Self::Read => diagnostic(
                "E_CONFIG_READ",
                ConfigDiagnosticStage::Read,
                "canonical configuration could not be read securely",
            ),
            Self::Lock => diagnostic(
                "E_CONFIG_LOCK",
                ConfigDiagnosticStage::Write,
                "canonical configuration transaction lock could not be acquired securely",
            ),
            Self::NotRegular => diagnostic(
                "E_CONFIG_FILE_TYPE",
                ConfigDiagnosticStage::Read,
                "canonical configuration must be a regular file and not a symbolic link",
            ),
            Self::TooLarge => diagnostic(
                "E_CONFIG_TOO_LARGE",
                ConfigDiagnosticStage::Read,
                "canonical configuration exceeds the one-MiB limit",
            ),
            Self::DirectoryChanged | Self::Unstable => diagnostic(
                "E_CONFIG_UNSTABLE",
                ConfigDiagnosticStage::Read,
                "canonical configuration changed during a stable read",
            ),
            Self::TempCreate | Self::TempWrite => diagnostic(
                "E_CONFIG_WRITE",
                ConfigDiagnosticStage::Write,
                "canonical configuration candidate could not be written",
            ),
            Self::FileSync => diagnostic(
                "E_CONFIG_FILE_SYNC",
                ConfigDiagnosticStage::Sync,
                "canonical configuration candidate could not be durably synced",
            ),
            Self::Rename => diagnostic(
                "E_CONFIG_RENAME",
                ConfigDiagnosticStage::Write,
                "canonical configuration candidate could not be atomically installed",
            ),
            Self::DirectorySync => diagnostic(
                "E_CONFIG_DIRECTORY_SYNC",
                ConfigDiagnosticStage::Sync,
                "canonical configuration directory could not be durably synced",
            ),
            Self::Rollback => diagnostic(
                "E_CONFIG_ROLLBACK",
                ConfigDiagnosticStage::Rollback,
                "canonical configuration replacement could not be safely rolled back",
            ),
        }
    }
}

pub(super) fn validate_path(path: &Path) -> Result<(), ConfigCoordinatorPathError> {
    let mut components = path.components();
    let has_normal_component =
        components.any(|component| matches!(component, Component::Normal(_)));
    if !has_normal_component || !matches!(path.file_name(), Some(name) if !name.is_empty()) {
        return Err(ConfigCoordinatorPathError::MissingFileName);
    }
    Ok(())
}

impl CanonicalStorage {
    pub(super) fn open(path: &Path) -> Result<Self, StorageFailure> {
        let file_name = path
            .file_name()
            .ok_or(StorageFailure::DirectoryOpen)?
            .to_owned();
        let parent_path = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };

        Ok(Self {
            file_name,
            directories: DirectoryChain::open(parent_path)?,
        })
    }

    pub(super) fn read(&self) -> Result<Vec<u8>, StorageFailure> {
        let snapshot = self.read_stable_name(&self.file_name)?;
        self.verify_directory_identity()?;
        Ok(snapshot.bytes)
    }

    pub(super) fn lock_transaction(&self) -> Result<TransactionLock<'_>, StorageFailure> {
        self.verify_directory_identity()?;
        let namespace = DirectoryNamespaceLock::acquire(self.directory())?;
        self.verify_directory_identity()?;

        let name = transaction_lock_name(&self.file_name);
        let (descriptor, created) = match fs::openat(
            self.directory(),
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            SECURE_FILE_MODE,
        ) {
            Ok(descriptor) => (descriptor, true),
            Err(Errno::EXIST) => (
                fs::openat(
                    self.directory(),
                    &name,
                    OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|_| StorageFailure::Lock)?,
                false,
            ),
            Err(_) => return Err(StorageFailure::Lock),
        };
        if created {
            fs::fchmod(&descriptor, SECURE_FILE_MODE).map_err(|_| StorageFailure::Lock)?;
        }
        validate_lock_descriptor(&descriptor).map_err(|()| StorageFailure::Lock)?;
        fs::flock(&descriptor, FlockOperation::LockExclusive).map_err(|_| StorageFailure::Lock)?;
        let metadata = fs::fstat(&descriptor).map_err(|_| StorageFailure::Lock)?;
        let transaction = TransactionLock {
            descriptor,
            name,
            identity: FileIdentity::new(metadata.st_dev, metadata.st_ino),
            _namespace: namespace,
        };
        transaction.verify(self)?;
        if created {
            fs::fsync(&transaction.descriptor).map_err(|_| StorageFailure::Lock)?;
            fs::fsync(self.directory()).map_err(|_| StorageFailure::Lock)?;
        }
        transaction.verify(self)?;
        Ok(transaction)
    }

    pub(super) fn replace<F, G>(
        &self,
        transaction: &TransactionLock<'_>,
        expected_revision: &ConfigRevision,
        candidate: &[u8],
        before_exchange: F,
        after_exchange: G,
        control: ReplaceControl,
    ) -> Result<ReplaceResult, StorageFailure>
    where
        F: FnOnce() -> Result<(), ()>,
        G: FnOnce(),
    {
        let PreparedExchange {
            mut temp,
            _candidate_file,
        } = match self.prepare_exchange(
            transaction,
            expected_revision,
            candidate,
            before_exchange,
            after_exchange,
            control,
        )? {
            PrepareResult::Conflict(disk_revision) => {
                return Ok(ReplaceResult::Conflict(disk_revision));
            }
            PrepareResult::Exchanged(exchange) => exchange,
        };

        let displaced = self.read_stable_name(temp.name())?;
        let installed = match self.read_stable_name(&self.file_name) {
            Ok(installed) => installed,
            Err(error) => {
                self.abandon_exchange(&mut temp, &displaced, control)?;
                return Err(error);
            }
        };

        if !installed.matches(temp.identity(), candidate) {
            let disk_revision = installed.revision();
            self.abandon_exchange(&mut temp, &displaced, control)?;
            return Ok(ReplaceResult::Conflict(disk_revision));
        }

        if let Err(error) = transaction.verify(self) {
            return match self.rollback(&mut temp, candidate, &displaced, control)? {
                RollbackResult::Restored => Err(error),
                RollbackResult::Conflict(disk_revision) => {
                    Ok(ReplaceResult::Conflict(disk_revision))
                }
            };
        }

        let displaced_revision = displaced.revision();
        if displaced_revision != *expected_revision {
            return match self.rollback(&mut temp, candidate, &displaced, control)? {
                RollbackResult::Restored => Ok(ReplaceResult::Conflict(displaced_revision)),
                RollbackResult::Conflict(disk_revision) => {
                    Ok(ReplaceResult::Conflict(disk_revision))
                }
            };
        }

        if self.sync_commit(control).is_err() {
            let installed = self.read_stable_name(&self.file_name)?;
            if !installed.matches(temp.identity(), candidate) {
                let disk_revision = installed.revision();
                self.abandon_exchange(&mut temp, &displaced, control)?;
                return Ok(ReplaceResult::Conflict(disk_revision));
            }
            return match self.rollback(&mut temp, candidate, &displaced, control)? {
                RollbackResult::Restored => Err(StorageFailure::DirectorySync),
                RollbackResult::Conflict(disk_revision) => {
                    Ok(ReplaceResult::Conflict(disk_revision))
                }
            };
        }

        let installed = self.read_stable_name(&self.file_name)?;
        if !installed.matches(temp.identity(), candidate) {
            let disk_revision = installed.revision();
            self.abandon_exchange(&mut temp, &displaced, control)?;
            return Ok(ReplaceResult::Conflict(disk_revision));
        }
        if self.read_stable_name(temp.name())? != displaced {
            return Err(StorageFailure::Rollback);
        }

        let cleanup_degraded = self
            .remove_exact_snapshot(
                &mut temp,
                &displaced,
                control,
                StorageFailure::DirectorySync,
            )
            .is_err();

        self.verify_directory_identity()?;
        let installed = self.read_stable_name(&self.file_name)?;
        if !installed.matches(temp.identity(), candidate) {
            return Ok(ReplaceResult::Conflict(installed.revision()));
        }

        Ok(ReplaceResult::Saved { cleanup_degraded })
    }

    fn prepare_exchange<F, G>(
        &self,
        transaction: &TransactionLock<'_>,
        expected_revision: &ConfigRevision,
        candidate: &[u8],
        before_exchange: F,
        after_exchange: G,
        control: ReplaceControl,
    ) -> Result<PrepareResult, StorageFailure>
    where
        F: FnOnce() -> Result<(), ()>,
        G: FnOnce(),
    {
        let (mut temp, candidate_file) = self.create_synced_candidate(candidate, control)?;
        let current = match self.read_stable_name(&self.file_name) {
            Ok(current) => current,
            Err(error) => {
                return self.finish_candidate(&mut temp, Some(candidate), control, Err(error));
            }
        };
        let current_revision = current.revision();
        if current_revision != *expected_revision {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Ok(PrepareResult::Conflict(current_revision)),
            );
        }
        if let Err(error) = transaction.verify(self) {
            return self.finish_candidate(&mut temp, Some(candidate), control, Err(error));
        }
        if before_exchange().is_err() {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Err(StorageFailure::TempWrite),
            );
        }
        #[cfg(test)]
        if control.fail_before_exchange {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Err(StorageFailure::TempWrite),
            );
        }

        if let Err(error) = transaction.verify(self) {
            return self.finish_candidate(&mut temp, Some(candidate), control, Err(error));
        }

        if self.exchange(temp.name(), &self.file_name).is_err() {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Err(StorageFailure::Rename),
            );
        }
        after_exchange();
        Ok(PrepareResult::Exchanged(PreparedExchange {
            temp,
            _candidate_file: candidate_file,
        }))
    }

    fn create_synced_candidate(
        &self,
        candidate: &[u8],
        control: ReplaceControl,
    ) -> Result<(TempEntry, File), StorageFailure> {
        let (mut temp, mut candidate_file) = self.create_temp()?;
        if candidate_file.write_all(candidate).is_err() {
            return self.finish_candidate(&mut temp, None, control, Err(StorageFailure::TempWrite));
        }
        if fs::fchmod(&candidate_file, SECURE_FILE_MODE).is_err() {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Err(StorageFailure::TempWrite),
            );
        }
        if fs::fsync(&candidate_file).is_err() {
            return self.finish_candidate(
                &mut temp,
                Some(candidate),
                control,
                Err(StorageFailure::FileSync),
            );
        }
        Ok((temp, candidate_file))
    }

    fn create_temp(&self) -> Result<(TempEntry, File), StorageFailure> {
        for _ in 0..MAX_TEMP_ATTEMPTS {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = OsString::from(format!(
                ".oxiroute-config.{}.{}.tmp",
                std::process::id(),
                sequence
            ));
            match fs::openat(
                self.directory(),
                &name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                SECURE_FILE_MODE,
            ) {
                Ok(descriptor) => {
                    let metadata =
                        fs::fstat(&descriptor).map_err(|_| StorageFailure::TempCreate)?;
                    let identity = FileIdentity::new(metadata.st_dev, metadata.st_ino);
                    return Ok((TempEntry::new(name, identity), File::from(descriptor)));
                }
                Err(Errno::EXIST) => {}
                Err(_) => return Err(StorageFailure::TempCreate),
            }
        }
        Err(StorageFailure::TempCreate)
    }

    fn read_stable_name(&self, name: &OsStr) -> Result<FileSnapshot, StorageFailure> {
        self.read_stable_name_with(name, || {})
    }

    fn read_stable_name_with<F>(
        &self,
        name: &OsStr,
        after_first: F,
    ) -> Result<FileSnapshot, StorageFailure>
    where
        F: FnOnce(),
    {
        let first = self.read_once(name)?;
        after_first();
        let second = self.read_once(name)?;
        if first != second {
            return Err(StorageFailure::Unstable);
        }
        Ok(first)
    }

    fn read_once(&self, name: &OsStr) -> Result<FileSnapshot, StorageFailure> {
        let descriptor = fs::openat(
            self.directory(),
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == Errno::LOOP {
                StorageFailure::NotRegular
            } else {
                StorageFailure::FileOpen
            }
        })?;
        let before = fs::fstat(&descriptor).map_err(|_| StorageFailure::Read)?;
        if !FileType::from_raw_mode(before.st_mode).is_file() {
            return Err(StorageFailure::NotRegular);
        }
        let size = usize::try_from(before.st_size).map_err(|_| StorageFailure::TooLarge)?;
        if size > MAX_CANONICAL_CONFIG_BYTES {
            return Err(StorageFailure::TooLarge);
        }

        let mut file = File::from(descriptor);
        let mut bytes = Vec::with_capacity(size);
        Read::by_ref(&mut file)
            .take((MAX_CANONICAL_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| StorageFailure::Read)?;
        if bytes.len() > MAX_CANONICAL_CONFIG_BYTES {
            return Err(StorageFailure::TooLarge);
        }
        let after = fs::fstat(&file).map_err(|_| StorageFailure::Read)?;
        if before.st_dev != after.st_dev
            || before.st_ino != after.st_ino
            || before.st_size != after.st_size
        {
            return Err(StorageFailure::Unstable);
        }

        Ok(FileSnapshot {
            identity: FileIdentity::new(before.st_dev, before.st_ino),
            bytes,
        })
    }

    fn finish_candidate<T>(
        &self,
        temp: &mut TempEntry,
        expected_bytes: Option<&[u8]>,
        control: ReplaceControl,
        result: Result<T, StorageFailure>,
    ) -> Result<T, StorageFailure> {
        let identity = temp.identity();
        self.remove_exact_identity(
            temp,
            identity,
            expected_bytes,
            control,
            StorageFailure::DirectorySync,
        )?;
        result
    }

    fn abandon_exchange(
        &self,
        temp: &mut TempEntry,
        displaced: &FileSnapshot,
        control: ReplaceControl,
    ) -> Result<(), StorageFailure> {
        self.remove_exact_snapshot(temp, displaced, control, StorageFailure::Rollback)
    }

    fn rollback(
        &self,
        temp: &mut TempEntry,
        candidate: &[u8],
        displaced: &FileSnapshot,
        control: ReplaceControl,
    ) -> Result<RollbackResult, StorageFailure> {
        let installed = self
            .read_stable_name(&self.file_name)
            .map_err(|_| StorageFailure::Rollback)?;
        let current_displaced = self
            .read_stable_name(temp.name())
            .map_err(|_| StorageFailure::Rollback)?;
        if !installed.matches(temp.identity(), candidate) {
            let disk_revision = installed.revision();
            self.abandon_exchange(temp, displaced, control)?;
            return Ok(RollbackResult::Conflict(disk_revision));
        }
        if current_displaced != *displaced {
            return Err(StorageFailure::Rollback);
        }

        self.exchange(temp.name(), &self.file_name)
            .map_err(|_| StorageFailure::Rollback)?;

        let restored = self
            .read_stable_name(&self.file_name)
            .map_err(|_| StorageFailure::Rollback)?;
        let removed_candidate = self
            .read_stable_name(temp.name())
            .map_err(|_| StorageFailure::Rollback)?;
        if restored == *displaced && !removed_candidate.matches(temp.identity(), candidate) {
            self.exchange(temp.name(), &self.file_name)
                .map_err(|_| StorageFailure::Rollback)?;
            let current = self
                .read_stable_name(&self.file_name)
                .map_err(|_| StorageFailure::Rollback)?;
            let current_displaced = self
                .read_stable_name(temp.name())
                .map_err(|_| StorageFailure::Rollback)?;
            if current != removed_candidate || current_displaced != *displaced {
                return Err(StorageFailure::Rollback);
            }
            let disk_revision = current.revision();
            self.abandon_exchange(temp, displaced, control)?;
            return Ok(RollbackResult::Conflict(disk_revision));
        }
        if restored != *displaced && removed_candidate.matches(temp.identity(), candidate) {
            let disk_revision = restored.revision();
            fs::fsync(self.directory()).map_err(|_| StorageFailure::Rollback)?;
            self.remove_exact_snapshot(
                temp,
                &removed_candidate,
                control,
                StorageFailure::Rollback,
            )?;
            return Ok(RollbackResult::Conflict(disk_revision));
        }
        if restored != *displaced || !removed_candidate.matches(temp.identity(), candidate) {
            return Err(StorageFailure::Rollback);
        }

        let rollback_sync = fs::fsync(self.directory());
        self.remove_exact_snapshot(temp, &removed_candidate, control, StorageFailure::Rollback)?;
        rollback_sync.map_err(|_| StorageFailure::Rollback)?;
        Ok(RollbackResult::Restored)
    }

    fn remove_exact_snapshot(
        &self,
        temp: &mut TempEntry,
        expected: &FileSnapshot,
        control: ReplaceControl,
        failure: StorageFailure,
    ) -> Result<(), StorageFailure> {
        self.remove_exact_identity(
            temp,
            expected.identity,
            Some(&expected.bytes),
            control,
            failure,
        )
    }

    fn remove_exact_identity(
        &self,
        temp: &mut TempEntry,
        expected_identity: FileIdentity,
        expected_bytes: Option<&[u8]>,
        control: ReplaceControl,
        failure: StorageFailure,
    ) -> Result<(), StorageFailure> {
        let current = self.read_stable_name(temp.name()).map_err(|_| failure)?;
        if current.identity != expected_identity
            || expected_bytes.is_some_and(|bytes| current.bytes != bytes)
        {
            return Err(failure);
        }
        fs::unlinkat(self.directory(), temp.name(), AtFlags::empty()).map_err(|_| failure)?;
        self.sync_cleanup(control).map_err(|_| failure)
    }

    fn verify_directory_identity(&self) -> Result<(), StorageFailure> {
        self.directories.verify()
    }

    fn exchange(&self, first: &OsStr, second: &OsStr) -> Result<(), Errno> {
        fs::renameat_with(
            self.directory(),
            first,
            self.directory(),
            second,
            RenameFlags::EXCHANGE,
        )
    }

    fn sync_commit(&self, control: ReplaceControl) -> Result<(), Errno> {
        #[cfg(test)]
        if control.fail_commit_sync {
            return Err(Errno::IO);
        }
        #[cfg(not(test))]
        let _ = control;
        fs::fsync(self.directory())
    }

    fn sync_cleanup(&self, control: ReplaceControl) -> Result<(), Errno> {
        #[cfg(test)]
        if control.fail_cleanup_sync {
            return Err(Errno::IO);
        }
        #[cfg(not(test))]
        let _ = control;
        fs::fsync(self.directory())
    }

    fn directory(&self) -> &OwnedFd {
        self.directories.directory()
    }

    #[cfg(test)]
    pub(super) fn read_with_hook<F>(&self, after_first: F) -> Result<Vec<u8>, StorageFailure>
    where
        F: FnOnce(),
    {
        self.read_stable_name_with(&self.file_name, after_first)
            .map(|snapshot| snapshot.bytes)
    }
}

fn transaction_lock_name(file_name: &OsStr) -> OsString {
    OsString::from(format!(
        ".oxiroute-config.{}.lock",
        ConfigRevision::from_bytes(file_name.as_bytes())
    ))
}

fn validate_lock_descriptor(descriptor: &OwnedFd) -> Result<(), ()> {
    let metadata = fs::fstat(descriptor).map_err(|_| ())?;
    if !FileType::from_raw_mode(metadata.st_mode).is_file()
        || metadata.st_mode & 0o7777 != SECURE_FILE_MODE_BITS
        || metadata.st_nlink != 1
    {
        return Err(());
    }
    Ok(())
}

struct DirectoryNamespaceLock<'a> {
    descriptor: &'a OwnedFd,
}

impl<'a> DirectoryNamespaceLock<'a> {
    fn acquire(descriptor: &'a OwnedFd) -> Result<Self, StorageFailure> {
        fs::flock(descriptor, FlockOperation::LockExclusive).map_err(|_| StorageFailure::Lock)?;
        Ok(Self { descriptor })
    }
}

impl Drop for DirectoryNamespaceLock<'_> {
    fn drop(&mut self) {
        let _ = fs::flock(self.descriptor, FlockOperation::Unlock);
    }
}

pub(super) struct TransactionLock<'a> {
    descriptor: OwnedFd,
    name: OsString,
    identity: FileIdentity,
    _namespace: DirectoryNamespaceLock<'a>,
}

impl TransactionLock<'_> {
    fn verify(&self, storage: &CanonicalStorage) -> Result<(), StorageFailure> {
        storage
            .verify_directory_identity()
            .map_err(|_| StorageFailure::Lock)?;
        validate_lock_descriptor(&self.descriptor).map_err(|()| StorageFailure::Lock)?;
        let descriptor = fs::fstat(&self.descriptor).map_err(|_| StorageFailure::Lock)?;
        let linked = fs::statat(storage.directory(), &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| StorageFailure::Lock)?;
        if !FileType::from_raw_mode(linked.st_mode).is_file()
            || linked.st_mode & 0o7777 != SECURE_FILE_MODE_BITS
            || linked.st_nlink != 1
            || FileIdentity::new(descriptor.st_dev, descriptor.st_ino) != self.identity
            || FileIdentity::new(linked.st_dev, linked.st_ino) != self.identity
        {
            return Err(StorageFailure::Lock);
        }
        Ok(())
    }
}

impl Drop for TransactionLock<'_> {
    fn drop(&mut self) {
        let _ = fs::flock(&self.descriptor, FlockOperation::Unlock);
    }
}

struct DirectoryChain {
    nodes: Vec<DirectoryNode>,
}

impl DirectoryChain {
    fn open(path: &Path) -> Result<Self, StorageFailure> {
        let anchor = if path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        };
        let descriptor = fs::open(anchor, DIRECTORY_FLAGS, Mode::empty())
            .map_err(|_| StorageFailure::DirectoryOpen)?;
        let metadata = fs::fstat(&descriptor).map_err(|_| StorageFailure::DirectoryOpen)?;
        let mut nodes = vec![DirectoryNode {
            descriptor,
            identity: FileIdentity::new(metadata.st_dev, metadata.st_ino),
            name_from_parent: None,
        }];

        for component in path.components() {
            let name = match component {
                Component::Prefix(_) | Component::RootDir | Component::CurDir => continue,
                Component::ParentDir => OsStr::new(".."),
                Component::Normal(name) => name,
            };
            let descriptor = fs::openat(
                &nodes.last().expect("directory anchor").descriptor,
                name,
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map_err(|_| StorageFailure::DirectoryOpen)?;
            let metadata = fs::fstat(&descriptor).map_err(|_| StorageFailure::DirectoryOpen)?;
            nodes.push(DirectoryNode {
                descriptor,
                identity: FileIdentity::new(metadata.st_dev, metadata.st_ino),
                name_from_parent: Some(name.to_owned()),
            });
        }

        let chain = Self { nodes };
        chain.verify()?;
        Ok(chain)
    }

    fn verify(&self) -> Result<(), StorageFailure> {
        for (index, node) in self.nodes.iter().enumerate() {
            let metadata =
                fs::fstat(&node.descriptor).map_err(|_| StorageFailure::DirectoryChanged)?;
            if !FileType::from_raw_mode(metadata.st_mode).is_dir()
                || FileIdentity::new(metadata.st_dev, metadata.st_ino) != node.identity
            {
                return Err(StorageFailure::DirectoryChanged);
            }
            if index == 0 {
                continue;
            }

            let parent = &self.nodes[index - 1];
            let name = node
                .name_from_parent
                .as_deref()
                .expect("traversed component");
            let linked = fs::statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| StorageFailure::DirectoryChanged)?;
            if !FileType::from_raw_mode(linked.st_mode).is_dir()
                || FileIdentity::new(linked.st_dev, linked.st_ino) != node.identity
            {
                return Err(StorageFailure::DirectoryChanged);
            }
        }
        Ok(())
    }

    fn directory(&self) -> &OwnedFd {
        &self.nodes.last().expect("directory anchor").descriptor
    }
}

struct DirectoryNode {
    descriptor: OwnedFd,
    identity: FileIdentity,
    name_from_parent: Option<OsString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    const fn new(device: u64, inode: u64) -> Self {
        Self { device, inode }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct FileSnapshot {
    identity: FileIdentity,
    bytes: Vec<u8>,
}

impl FileSnapshot {
    fn matches(&self, identity: FileIdentity, bytes: &[u8]) -> bool {
        self.identity == identity && self.bytes == bytes
    }

    fn revision(&self) -> ConfigRevision {
        ConfigRevision::from_bytes(&self.bytes)
    }
}

struct TempEntry {
    name: OsString,
    identity: FileIdentity,
}

impl TempEntry {
    fn new(name: OsString, identity: FileIdentity) -> Self {
        Self { name, identity }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }

    const fn identity(&self) -> FileIdentity {
        self.identity
    }
}
