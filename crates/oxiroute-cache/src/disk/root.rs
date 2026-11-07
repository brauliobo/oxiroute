#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_disk_config(config: &DiskCacheConfig) -> Result<(), DiskCacheError> {
    if config.max_disk_bytes == 0
        || config.max_disk_files == 0
        || config.max_record_bytes == 0
        || u64::try_from(config.max_record_bytes)
            .map_or(true, |maximum| maximum > config.max_disk_bytes)
    {
        return Err(DiskCacheError::InvalidConfig);
    }
    Ok(())
}

pub(super) fn open_or_create_root(path: &Path) -> Result<OwnedFd, DiskCacheError> {
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(DiskCacheError::RootOpen(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty cache root",
        )));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory = rustix_fs::open(anchor, flags, Mode::empty())
        .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
    let normals = components
        .iter()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(*name),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(DiskCacheError::RootOpen(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent traversal is not allowed",
        )));
    }
    for (index, name) in normals.iter().enumerate() {
        match rustix_fs::openat(&directory, *name, flags, Mode::empty()) {
            Ok(next) => directory = next,
            Err(Errno::NOENT) if index + 1 == normals.len() => {
                rustix_fs::mkdirat(&directory, *name, Mode::from_raw_mode(ROOT_MODE))
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
                rustix_fs::fsync(&directory)
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
                directory = rustix_fs::openat(&directory, *name, flags, Mode::empty())
                    .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
            }
            Err(source) => return Err(DiskCacheError::RootOpen(source.into())),
        }
    }
    Ok(directory)
}

pub(super) fn validate_root_access(path: &Path) -> Result<(), DiskCacheError> {
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(DiskCacheError::RootOpen(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid cache root",
        )));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory = rustix_fs::open(anchor, flags, Mode::empty())
        .map_err(|source| DiskCacheError::RootOpen(source.into()))?;
    let normals = components
        .iter()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(*name),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, name) in normals.iter().enumerate() {
        match rustix_fs::openat(&directory, *name, flags, Mode::empty()) {
            Ok(next) => directory = next,
            Err(Errno::NOENT) if index + 1 == normals.len() => {
                return rustix_fs::accessat(
                    &directory,
                    ".",
                    rustix_fs::Access::WRITE_OK,
                    AtFlags::EACCESS,
                )
                .map_err(|source| DiskCacheError::RootOpen(source.into()));
            }
            Err(source) => return Err(DiskCacheError::RootOpen(source.into())),
        }
    }
    verify_root_access(&directory)
}

fn verify_root_access(root: &OwnedFd) -> Result<(), DiskCacheError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| DiskCacheError::RootControl)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_dir() || metadata.st_mode & 0o777 != ROOT_MODE
    {
        return Err(DiskCacheError::RootControl);
    }
    rustix_fs::accessat(root, ".", rustix_fs::Access::WRITE_OK, AtFlags::EACCESS)
        .map_err(|_| DiskCacheError::RootControl)
}

pub(super) fn verify_root_control(root: &OwnedFd) -> Result<u32, DiskCacheError> {
    let metadata = rustix_fs::fstat(root).map_err(|_| DiskCacheError::RootControl)?;
    if !FileType::from_raw_mode(metadata.st_mode).is_dir() || metadata.st_mode & 0o777 != ROOT_MODE
    {
        return Err(DiskCacheError::RootControl);
    }
    for _ in 0..MAX_NAME_ATTEMPTS {
        let name = format!("{OWNER_PROBE_PREFIX}{}", Uuid::new_v4().simple());
        match rustix_fs::openat(
            root,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(probe) => {
                let probe = rustix_fs::fstat(&probe).map_err(|_| DiskCacheError::RootControl)?;
                rustix_fs::unlinkat(root, &name, AtFlags::empty())
                    .map_err(|_| DiskCacheError::RootControl)?;
                if probe.st_uid != metadata.st_uid {
                    return Err(DiskCacheError::RootControl);
                }
                return Ok(metadata.st_uid);
            }
            Err(Errno::EXIST) => {}
            Err(_) => return Err(DiskCacheError::RootControl),
        }
    }
    Err(DiskCacheError::RootControl)
}

pub(super) fn open_ownership_lock(root: &OwnedFd, owner: u32) -> Result<OwnedFd, DiskCacheError> {
    let (descriptor, created) = match rustix_fs::openat(
        root,
        LOCK_NAME,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(descriptor) => (descriptor, true),
        Err(Errno::EXIST) => (
            rustix_fs::openat(
                root,
                LOCK_NAME,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?,
            false,
        ),
        Err(source) => return Err(DiskCacheError::OwnershipOpen(source.into())),
    };
    if created {
        rustix_fs::fchmod(&descriptor, Mode::from_raw_mode(FILE_MODE))
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    }
    let metadata = rustix_fs::fstat(&descriptor)
        .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    let linked = rustix_fs::statat(root, LOCK_NAME, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    if !secure_file(&metadata, owner)
        || metadata.st_dev != linked.st_dev
        || metadata.st_ino != linked.st_ino
    {
        return Err(DiskCacheError::OwnershipOpen(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership lock identity, type, owner, links, or mode is invalid",
        )));
    }
    if created {
        rustix_fs::fsync(&descriptor)
            .and_then(|()| rustix_fs::fsync(root))
            .map_err(|source| DiskCacheError::OwnershipOpen(source.into()))?;
    }
    Ok(descriptor)
}

#[allow(clippy::too_many_lines)]
pub(super) fn scan_root(
    root: &OwnedFd,
    owner: u32,
    config: &DiskCacheConfig,
    cache: &Cache,
) -> Result<State, DiskCacheError> {
    let mut directory =
        Dir::read_from(root).map_err(|source| DiskCacheError::RootRead(source.into()))?;
    let mut candidates: HashMap<CacheKey, (DecodedRecord, RecordInfo)> = HashMap::new();
    let mut cleaned = false;
    let mut stale_temps_removed = 0u64;
    let mut corrupt_records_removed = 0u64;
    for entry in &mut directory {
        let entry = entry.map_err(|source| DiskCacheError::RootRead(source.into()))?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") || raw == LOCK_NAME.as_bytes() || is_owner_probe(raw) {
            continue;
        }
        let Ok(name) = std::str::from_utf8(raw) else {
            continue;
        };
        let temp = is_temp_name(raw);
        let record = is_record_name(raw);
        if !temp && !record {
            continue;
        }
        let metadata = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(Errno::NOENT) => continue,
            Err(source) => return Err(storage(source)),
        };
        if temp {
            remove_namespace_entry(root, name, &metadata)?;
            stale_temps_removed = stale_temps_removed.saturating_add(1);
            cleaned = true;
            continue;
        }
        let decoded = read_record(root, name, &metadata, owner, config, cache);
        let Ok(decoded) = decoded else {
            remove_namespace_entry(root, name, &metadata)?;
            corrupt_records_removed = corrupt_records_removed.saturating_add(1);
            cleaned = true;
            continue;
        };
        let size = u64::try_from(metadata.st_size).map_err(|_| DiskCacheError::RecordTooLarge)?;
        let info = RecordInfo {
            name: name.to_owned(),
            size,
            device: metadata.st_dev,
            inode: metadata.st_ino,
            access: decoded.access,
            sequence: decoded.sequence,
            tags: decoded.entry.tags.clone(),
        };
        match candidates.entry(decoded.entry.key.clone()) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((decoded, info));
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let old_order = (
                    slot.get().0.access,
                    slot.get().0.sequence,
                    slot.get().1.name.as_str(),
                );
                let new_order = (decoded.access, decoded.sequence, info.name.as_str());
                if new_order > old_order {
                    let old = &slot.get().1;
                    remove_owned_name(root, &old.name, Some((old.device, old.inode)))?;
                    slot.insert((decoded, info));
                } else {
                    remove_owned_name(root, name, Some((metadata.st_dev, metadata.st_ino)))?;
                }
                corrupt_records_removed = corrupt_records_removed.saturating_add(1);
                cleaned = true;
            }
        }
    }

    let mut ordered = candidates.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (left.0.access, left.0.sequence, &left.0.entry.key).cmp(&(
            right.0.access,
            right.0.sequence,
            &right.0.entry.key,
        ))
    });
    while ordered.len() > config.max_disk_files
        || ordered.iter().map(|(_, info)| info.size).sum::<u64>() > config.max_disk_bytes
    {
        let (_, info) = ordered.remove(0);
        remove_owned_name(root, &info.name, Some((info.device, info.inode)))?;
        corrupt_records_removed = corrupt_records_removed.saturating_add(1);
        cleaned = true;
    }
    if cleaned {
        rustix_fs::fsync(root).map_err(storage)?;
    }
    let mut state = State {
        records: HashMap::new(),
        tags: HashMap::new(),
        bytes_used: 0,
        sequence: 0,
        recovered: ordered.len() as u64,
        stale_temps_removed,
        corrupt_records_removed,
    };
    let mut memory_removed = 0usize;
    for (decoded, info) in ordered {
        state.sequence = state.sequence.max(decoded.sequence).max(decoded.access);
        let key = decoded.entry.key.clone();
        let delta = cache.restore(decoded.entry);
        state.insert(key, info);
        memory_removed =
            memory_removed.saturating_add(reconcile_insertion(root, &mut state, &delta)?);
    }
    if memory_removed != 0 {
        rustix_fs::fsync(root).map_err(storage)?;
    }
    Ok(state)
}

pub(super) fn reconcile_insertion(
    root: &OwnedFd,
    state: &mut State,
    delta: &InsertionDelta,
) -> Result<usize, DiskCacheError> {
    let mut removed = 0usize;
    for key in &delta.removed {
        if key == &delta.inserted {
            continue;
        }
        if let Some(record) = state.records.get(key).cloned() {
            remove_owned_name(root, &record.name, Some((record.device, record.inode)))?;
            state.remove(key);
            removed += 1;
        }
    }
    Ok(removed)
}

pub(super) fn verify_named_identity(
    root: &OwnedFd,
    name: &str,
    descriptor: &rustix_fs::Stat,
    owner: u32,
) -> Result<(), DiskCacheError> {
    let linked = rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW).map_err(storage)?;
    if !secure_file(descriptor, owner)
        || descriptor.st_dev != linked.st_dev
        || descriptor.st_ino != linked.st_ino
    {
        return Err(DiskCacheError::UnsafeEntry);
    }
    Ok(())
}

pub(super) fn remove_owned_name(
    root: &OwnedFd,
    name: &str,
    expected: Option<(u64, u64)>,
) -> Result<(), DiskCacheError> {
    let current = match rustix_fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(Errno::NOENT) => return Ok(()),
        Err(source) => return Err(storage(source)),
    };
    if expected.is_some_and(|(device, inode)| current.st_dev != device || current.st_ino != inode) {
        return Err(DiskCacheError::UnsafeEntry);
    }
    rustix_fs::unlinkat(root, name, AtFlags::empty()).map_err(storage)
}

fn remove_namespace_entry(
    root: &OwnedFd,
    name: &str,
    metadata: &rustix_fs::Stat,
) -> Result<(), DiskCacheError> {
    if FileType::from_raw_mode(metadata.st_mode).is_dir() {
        return Err(DiskCacheError::UnsafeEntry);
    }
    remove_owned_name(root, name, Some((metadata.st_dev, metadata.st_ino)))
}

pub(super) fn secure_file(metadata: &rustix_fs::Stat, owner: u32) -> bool {
    FileType::from_raw_mode(metadata.st_mode).is_file()
        && metadata.st_uid == owner
        && metadata.st_mode & 0o777 == FILE_MODE
        && metadata.st_nlink == 1
}

pub(super) fn is_temp_name(name: &[u8]) -> bool {
    uuid_name(name, TEMP_SUFFIX.as_bytes())
}

fn is_record_name(name: &[u8]) -> bool {
    let Some(middle) = name
        .strip_prefix(RECORD_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(RECORD_SUFFIX.as_bytes()))
    else {
        return false;
    };
    middle.len() == 16 + 1 + 32
        && middle[16] == b'-'
        && middle[..16].iter().all(u8::is_ascii_hexdigit)
        && middle[17..].iter().all(u8::is_ascii_hexdigit)
}

fn uuid_name(name: &[u8], suffix: &[u8]) -> bool {
    let Some(token) = name
        .strip_prefix(TEMP_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(suffix))
    else {
        return false;
    };
    token.len() == 32 && token.iter().all(u8::is_ascii_hexdigit)
}

fn is_owner_probe(name: &[u8]) -> bool {
    name.strip_prefix(OWNER_PROBE_PREFIX.as_bytes())
        .is_some_and(|token| token.len() == 32 && token.iter().all(u8::is_ascii_hexdigit))
}

pub(super) fn storage(source: impl Into<io::Error>) -> DiskCacheError {
    DiskCacheError::Storage(source.into())
}

pub(super) fn invalid_record() -> DiskCacheError {
    DiskCacheError::Storage(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid persistent cache record",
    ))
}
