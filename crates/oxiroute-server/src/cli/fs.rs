use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::Path,
};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};

use super::error::CliError;

pub(super) const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum FileError {
    Io(io::Error),
    NotRegular,
    Mode(u32),
    TooLarge(usize),
    Changed,
}

impl FileError {
    pub(super) fn is_missing(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == io::ErrorKind::NotFound)
    }
}

impl From<io::Error> for FileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rustix::io::Errno> for FileError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error)
                if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
            {
                formatter.write_str("is a symbolic link, which is refused")
            }
            Self::Io(error) => error.fmt(formatter),
            Self::NotRegular => formatter.write_str("is not a regular file"),
            Self::Mode(mode) => write!(formatter, "has mode {mode:04o}, expected 0400 or 0600"),
            Self::TooLarge(max) => write!(formatter, "is larger than {max} bytes"),
            Self::Changed => formatter.write_str("changed while it was read"),
        }
    }
}

pub(super) fn ensure_readable_config(path: &Path) -> Result<(), CliError> {
    File::open(path)
        .and_then(|file| file.metadata())
        .map_err(FileError::from)
        .and_then(|metadata| {
            if metadata.is_file() {
                Ok(())
            } else {
                Err(FileError::NotRegular)
            }
        })
        .map_err(|error| unreadable_config(path, &error))
}

pub(super) fn unreadable_config(path: &Path, error: &dyn fmt::Display) -> CliError {
    CliError::Local(format!(
        "cannot read configuration {}: {error}",
        path.display()
    ))
}

pub(super) fn read_regular_file(
    path: &Path,
    max_bytes: usize,
    secure_mode: bool,
) -> Result<Vec<u8>, FileError> {
    let descriptor = rustix_fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let before = rustix_fs::fstat(&descriptor)?;
    if !FileType::from_raw_mode(before.st_mode).is_file() {
        return Err(FileError::NotRegular);
    }
    let mode = before.st_mode & 0o7777;
    if secure_mode && !matches!(mode, 0o400 | 0o600) {
        return Err(FileError::Mode(mode));
    }
    let size = usize::try_from(before.st_size).map_err(|_| FileError::TooLarge(max_bytes))?;
    if size > max_bytes {
        return Err(FileError::TooLarge(max_bytes));
    }
    let mut file = File::from(descriptor);
    let mut bytes = Vec::with_capacity(size);
    io::Read::by_ref(&mut file)
        .take(u64::try_from(max_bytes + 1).expect("file bound fits u64"))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(FileError::TooLarge(max_bytes));
    }
    let after = rustix_fs::fstat(&file)?;
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_size != after.st_size
        || before.st_mode != after.st_mode
    {
        return Err(FileError::Changed);
    }
    Ok(bytes)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = directory.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    written
        .and_then(|()| fs::rename(&temporary, path))
        .and_then(|()| File::open(directory)?.sync_all())
        .inspect_err(|_| drop(fs::remove_file(&temporary)))
}
