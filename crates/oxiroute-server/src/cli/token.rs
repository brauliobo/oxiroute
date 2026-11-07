#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn command_requires_token(command: &Command) -> bool {
    !matches!(
        command,
        Command::Ready
            | Command::Metrics
            | Command::Check(_)
            | Command::Rtmp {
                command: RtmpCommand::Stream {
                    command: StreamCommand::Disconnect { .. },
                } | RtmpCommand::Relay {
                    command: RelayCommand::Reconnect { .. },
                },
            }
    )
}

pub(super) fn resolve_token_path(
    explicit_path: Option<&Path>,
    configured_path: Option<&Path>,
    package_env_file: &Path,
    default_path: &Path,
) -> Result<Option<PathBuf>, CliError> {
    if let Some(path) = explicit_path.or(configured_path) {
        return Ok(Some(path.to_owned()));
    }
    if let Some(path) = package_env_token_path(package_env_file)? {
        return Ok(Some(path));
    }
    match std::fs::symlink_metadata(default_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Ok(_) | Err(_) => Ok(Some(default_path.to_owned())),
    }
}

fn package_env_token_path(path: &Path) -> Result<Option<PathBuf>, CliError> {
    let bytes = match read_regular_file(path, MAX_PACKAGE_ENV_BYTES, false) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(CliError::Local(
                "management token path configuration could not be read securely".into(),
            ));
        }
    };
    let contents = std::str::from_utf8(&bytes)
        .map_err(|_| CliError::Local("management token path configuration is invalid".into()))?;
    let mut token_path = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name != MANAGEMENT_TOKEN_ENV {
            continue;
        }
        if token_path.is_some() {
            return Err(CliError::Local(
                "management token path configuration is invalid".into(),
            ));
        }
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(CliError::Local(
                "management token path configuration is invalid".into(),
            ));
        }
        token_path = Some(PathBuf::from(value));
    }
    Ok(token_path)
}

pub(super) fn read_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    let bytes = Zeroizing::new(
        read_regular_file(path, MAX_TOKEN_BYTES + 2, true).map_err(|_| {
            CliError::Local("management token file could not be read securely".into())
        })?,
    );
    let mut token = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CliError::Local("management token file is invalid".into()))?,
    );
    if token.ends_with("\r\n") {
        let length = token.len();
        token.truncate(length - 2);
    } else if token.ends_with('\n') {
        token.pop();
    }
    if token.len() < 32
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(CliError::Local("management token file is invalid".into()));
    }
    Ok(token)
}

pub(super) fn read_regular_file(
    path: &Path,
    max_bytes: usize,
    secure_mode: bool,
) -> io::Result<Vec<u8>> {
    let descriptor = rustix_fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let before = rustix_fs::fstat(&descriptor)?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || (secure_mode && !matches!(before.st_mode & 0o7777, 0o400 | 0o600))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file does not meet the required policy",
        ));
    }
    let size = usize::try_from(before.st_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is too large"))?;
    if size > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is too large",
        ));
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(size);
    io::Read::by_ref(&mut file)
        .take(u64::try_from(max_bytes + 1).expect("file bound fits u64"))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is too large",
        ));
    }
    let after = rustix_fs::fstat(&file)?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
        || before.st_mode != after.st_mode
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed while it was read",
        ));
    }
    Ok(bytes)
}
