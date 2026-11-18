use std::{
    fs::File,
    io::{self, Read as _},
    path::Path,
};

use rustix::fs::{self as rustix_fs, FileType, Mode, OFlags};

pub(super) const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

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
