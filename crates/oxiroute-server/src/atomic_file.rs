use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = directory.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path))
        .and_then(|()| File::open(directory)?.sync_all())
        .inspect_err(|_| drop(fs::remove_file(&temporary)))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn a_replaced_file_has_the_new_bytes_the_requested_mode_and_no_leftover() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("state.json");
        write_atomic(&path, b"first", 0o600).expect("first write");
        write_atomic(&path, b"second", 0o600).expect("second write");

        assert_eq!(fs::read(&path).expect("read"), b"second");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let names = fs::read_dir(directory.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["state.json"]);
    }
}
