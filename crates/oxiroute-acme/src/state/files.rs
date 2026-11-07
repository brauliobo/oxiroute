#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn ensure_state_root(path: &Path) -> Result<(), AcmeStateError> {
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

pub(super) fn ensure_managed_directory(root: &Path, path: &Path) -> Result<(), AcmeStateError> {
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

pub(super) fn shared_lock(root: &Path, path: &Path) -> Result<Arc<File>, AcmeStateError> {
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

pub(super) fn open_private_temp(path: &Path) -> io::Result<File> {
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

pub(super) fn write_and_sync(
    file: &mut File,
    bytes: &[u8],
    mode: u32,
) -> Result<(), AcmeStateError> {
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

pub(super) fn read_once(path: &Path, limit: usize) -> Result<Vec<u8>, AcmeStateError> {
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

pub(super) fn sync_directory(path: &Path) -> Result<(), AcmeStateError> {
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
