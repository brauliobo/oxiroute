#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn scan_root(
    root: &OwnedFd,
    limits: RecordingStoreLimits,
    clean_partials: bool,
) -> Result<StoreState, RecordingStoreError> {
    let mut directory =
        Dir::read_from(root).map_err(|source| RecordingStoreError::RootRead(source.into()))?;
    let mut state = StoreState::default();
    let mut cleaned = false;

    for entry in &mut directory {
        let entry = entry.map_err(|source| RecordingStoreError::RootRead(source.into()))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..")
            || name.to_bytes() == OWNERSHIP_LOCK_NAME.as_bytes()
            || is_owner_probe(name.to_bytes())
        {
            continue;
        }
        let metadata = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => continue,
            Err(source) => return Err(RecordingStoreError::RootEntryMetadata(source.into())),
        };
        let is_regular = FileType::from_raw_mode(metadata.st_mode).is_file();
        if clean_partials && is_regular && is_owned_partial(name.to_bytes()) {
            match rustix_fs::unlinkat(root, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {
                    cleaned = true;
                    continue;
                }
                Err(source) => return Err(RecordingStoreError::PartialCleanup(source.into())),
            }
        }
        if !is_regular {
            continue;
        }

        state.files =
            state
                .files
                .checked_add(1)
                .ok_or(RecordingStoreError::ExistingUsageExceedsLimits {
                    bytes_used: u64::MAX,
                    files: usize::MAX,
                })?;
        let size = u64::try_from(metadata.st_size).unwrap_or(u64::MAX);
        state.bytes_used = state.bytes_used.saturating_add(size);
    }

    if cleaned {
        rustix_fs::fsync(root).map_err(|source| RecordingStoreError::RootSync(source.into()))?;
    }
    if limits
        .max_bytes
        .is_some_and(|maximum| state.bytes_used > maximum)
        || limits
            .max_files
            .is_some_and(|maximum| state.files > maximum)
    {
        return Err(RecordingStoreError::ExistingUsageExceedsLimits {
            bytes_used: state.bytes_used,
            files: state.files,
        });
    }
    Ok(state)
}

pub(super) fn validate_relative_name(name: &str) -> Result<(), RecordingStoreError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > MAX_RECORDING_FILENAME_BYTES
        || name.as_bytes().contains(&0)
        || name.as_bytes().contains(&b'/')
        || name == OWNERSHIP_LOCK_NAME
        || is_owned_partial(name.as_bytes())
        || is_owner_probe(name.as_bytes())
    {
        return Err(RecordingStoreError::InvalidRelativeName);
    }
    Ok(())
}

pub(super) fn create_recording_entry(
    shared: &StoreShared,
    claim: &StoreOperationClaim,
    final_relative_name: &str,
    lock: bool,
) -> Result<(OwnedFd, String), RecordingStoreError> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let Some(relative_name) = (if attempt == 0 {
            Some(final_relative_name.to_owned())
        } else {
            collision_recording_filename(final_relative_name, attempt)
        }) else {
            break;
        };
        match rustix_fs::openat(
            &shared.root,
            relative_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(descriptor) => {
                if lock
                    && let Err(source) =
                        rustix_fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive)
                {
                    drop(descriptor);
                    unlink_created_entry(shared, &relative_name)?;
                    return Err(RecordingStoreError::PartialCreate(source.into()));
                }
                if !claim.incarnation_active() {
                    drop(descriptor);
                    unlink_created_entry(shared, &relative_name)?;
                    return Err(RecordingStoreError::CreationCancelled);
                }
                return Ok((descriptor, relative_name));
            }
            Err(Errno::EXIST) => {}
            Err(source) => return Err(RecordingStoreError::PartialCreate(source.into())),
        }
    }
    Err(RecordingStoreError::FinalNameCollisions {
        partial_relative_name: final_relative_name.to_owned(),
    })
}

fn unlink_created_entry(
    shared: &StoreShared,
    relative_name: &str,
) -> Result<(), RecordingStoreError> {
    match rustix_fs::unlinkat(&shared.root, relative_name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(cleanup) => Err(RecordingStoreError::PartialCleanup(cleanup.into())),
    }
}

fn is_owned_partial(name: &[u8]) -> bool {
    let Some(token) = name
        .strip_prefix(PARTIAL_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(PARTIAL_SUFFIX.as_bytes()))
    else {
        return false;
    };
    token.len() == UUID_SIMPLE_LENGTH
        && token
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_owner_probe(name: &[u8]) -> bool {
    let Some(token) = name.strip_prefix(OWNER_PROBE_PREFIX.as_bytes()) else {
        return false;
    };
    token.len() == UUID_SIMPLE_LENGTH
        && token
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(super) fn open_pinned_directory(path: &Path) -> Result<OwnedFd, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory = rustix_fs::open(
        if path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        },
        flags,
        Mode::empty(),
    )?;

    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                directory = rustix_fs::openat(&directory, name, flags, Mode::empty())?;
            }
            Component::ParentDir | Component::Prefix(_) => return Err(Errno::INVAL),
        }
    }
    Ok(directory)
}

pub(super) fn verify_root_control(root: &OwnedFd) -> Result<rustix_fs::Stat, RecordingStoreError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| RecordingStoreError::RootNotExclusive)?;
    if metadata.st_mode & 0o022 != 0 {
        return Err(RecordingStoreError::RootNotExclusive);
    }

    for _ in 0..MAX_NAME_ATTEMPTS {
        let probe_name = format!("{OWNER_PROBE_PREFIX}{}", Uuid::new_v4().simple());
        match rustix_fs::openat(
            root,
            probe_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(probe) => {
                let probe_metadata =
                    rustix_fs::fstat(&probe).map_err(|_| RecordingStoreError::RootNotExclusive)?;
                rustix_fs::unlinkat(root, probe_name.as_str(), AtFlags::empty())
                    .map_err(|_| RecordingStoreError::RootNotExclusive)?;
                if metadata.st_uid != probe_metadata.st_uid {
                    return Err(RecordingStoreError::RootNotExclusive);
                }
                return Ok(metadata);
            }
            Err(Errno::EXIST) => {}
            Err(_) => return Err(RecordingStoreError::RootNotExclusive),
        }
    }
    Err(RecordingStoreError::RootNotExclusive)
}

pub(super) fn verify_root_control_read_only(root: &OwnedFd) -> Result<(), RecordingStoreError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| RecordingStoreError::RootNotExclusive)?;
    let process =
        rustix_fs::stat("/proc/self").map_err(|_| RecordingStoreError::RootNotExclusive)?;
    if metadata.st_uid != process.st_uid || metadata.st_mode & 0o022 != 0 {
        return Err(RecordingStoreError::RootNotExclusive);
    }
    Ok(())
}

pub(super) fn acquire_shared_ownership(
    root: &OwnedFd,
    cancelled: &impl Fn() -> bool,
) -> Result<OwnedFd, RecordingStoreError> {
    let ownership = rustix_fs::openat(
        root,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|source| RecordingStoreError::OwnershipOpen(source.into()))?;
    loop {
        if cancelled() {
            return Err(RecordingStoreError::CreationCancelled);
        }
        match rustix_fs::flock(&ownership, FlockOperation::NonBlockingLockShared) {
            Ok(()) => return Ok(ownership),
            Err(source) if source == Errno::AGAIN || source == Errno::WOULDBLOCK => {
                thread::sleep(OWNERSHIP_RETRY_INTERVAL);
            }
            Err(source) => return Err(RecordingStoreError::OwnershipLock(source.into())),
        }
    }
}

pub(super) fn byte_quota_error(maximum: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::StorageFull,
        format!("recording byte limit of {maximum} reached"),
    )
}
