#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn read_bounded_stable(
    owner: &str,
    kind: &'static str,
    path: &Path,
    limit: usize,
    private_key: bool,
) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
    let first = read_bounded_once(owner, kind, path, limit, private_key)?;
    let second = read_bounded_once(owner, kind, path, limit, private_key)?;
    if first.as_slice() != second.as_slice() {
        return Err(TlsBuildError::FileChanged {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    Ok(first)
}

fn read_bounded_once(
    owner: &str,
    kind: &'static str,
    path: &Path,
    limit: usize,
    private_key: bool,
) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
    let mut file = open_read_no_follow(path).map_err(|source| TlsBuildError::FileOpen {
        owner: owner.into(),
        kind,
        path: path.into(),
        source,
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| TlsBuildError::FileMetadata {
            owner: owner.into(),
            kind,
            path: path.into(),
            source,
        })?;
    if !metadata.file_type().is_file() {
        return Err(TlsBuildError::NotRegularFile {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    if metadata.len() > limit as u64 {
        return Err(TlsBuildError::FileTooLarge {
            owner: owner.into(),
            kind,
            path: path.into(),
            limit,
        });
    }
    #[cfg(unix)]
    if private_key {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = metadata.permissions().mode() & 0o7777;
        if !matches!(mode, 0o400 | 0o600 | 0o440 | 0o640) {
            return Err(TlsBuildError::InsecurePrivateKeyPermissions {
                certificate: owner.into(),
                path: path.into(),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = private_key;

    let capacity = usize::try_from(metadata.len()).map_err(|_| TlsBuildError::FileTooLarge {
        owner: owner.into(),
        kind,
        path: path.into(),
        limit,
    })?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    file.by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| TlsBuildError::FileRead {
            owner: owner.into(),
            kind,
            path: path.into(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(TlsBuildError::FileTooLarge {
            owner: owner.into(),
            kind,
            path: path.into(),
            limit,
        });
    }

    if bytes.is_empty() {
        return Err(TlsBuildError::EmptyFile {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    Ok(bytes)
}

fn open_read_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    #[cfg(not(unix))]
    {
        File::open(path)
    }
}
