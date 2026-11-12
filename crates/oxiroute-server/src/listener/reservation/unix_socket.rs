//! Ownership of a Unix socket path: the lease marker file, the identity of what was bound, and the removal of a stale
//! socket left by a process that is gone.
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) struct UnixSocketIdentity {
    pub(super) path_identity: PathIdentity,
    pub(super) _lease: UnixSocketLease,
}

pub(super) struct PathIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) path: PathBuf,
    pub(super) socket: bool,
}

impl PathIdentity {
    pub(super) fn remove_if_unchanged(&self) {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if (self.socket && metadata.file_type().is_socket()
            || !self.socket && metadata.file_type().is_file())
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) struct UnixSocketLease {
    _descriptor: rustix::fd::OwnedFd,
    marker_identity: PathIdentity,
}

impl UnixSocketLease {
    pub(super) fn acquire(path: &Path) -> io::Result<Self> {
        use rustix::{
            fs::{self, FileType, FlockOperation, Mode, OFlags},
            io::Errno,
        };
        validate_unix_socket_parent(path)?;
        let marker_path = unix_socket_marker_path(path)?;
        let secure_mode = Mode::from_raw_mode(0o600);
        let (descriptor, created) = match fs::open(
            &marker_path,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            secure_mode,
        ) {
            Ok(descriptor) => (descriptor, true),
            Err(Errno::EXIST) => (
                fs::open(
                    &marker_path,
                    OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?,
                false,
            ),
            Err(source) => return Err(source.into()),
        };
        if created {
            fs::fchmod(&descriptor, secure_mode).map_err(io::Error::from)?;
        }
        let descriptor_metadata = fs::fstat(&descriptor).map_err(io::Error::from)?;
        let linked_metadata = fs::stat(&marker_path).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(descriptor_metadata.st_mode).is_file()
            || descriptor_metadata.st_mode & 0o7777 != 0o600
            || descriptor_metadata.st_nlink != 1
            || descriptor_metadata.st_dev != linked_metadata.st_dev
            || descriptor_metadata.st_ino != linked_metadata.st_ino
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Unix socket ownership marker `{}` is not a secure regular file",
                    marker_path.display()
                ),
            ));
        }
        match fs::flock(&descriptor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::AGAIN) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("Unix socket `{}` is already reserved", path.display()),
                ));
            }
            Err(source) => return Err(source.into()),
        }
        let linked_metadata = fs::stat(&marker_path).map_err(io::Error::from)?;
        if descriptor_metadata.st_dev != linked_metadata.st_dev
            || descriptor_metadata.st_ino != linked_metadata.st_ino
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Unix socket ownership marker `{}` changed while it was acquired",
                    marker_path.display()
                ),
            ));
        }
        Ok(Self {
            _descriptor: descriptor,
            marker_identity: PathIdentity {
                device: linked_metadata.st_dev,
                inode: linked_metadata.st_ino,
                path: marker_path,
                socket: false,
            },
        })
    }
}

impl Drop for UnixSocketLease {
    fn drop(&mut self) {
        self.marker_identity.remove_if_unchanged();
    }
}

fn validate_unix_socket_parent(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let effective_user = rustix::process::geteuid().as_raw();
    for (index, ancestor) in parent.ancestors().enumerate() {
        let metadata = std::fs::metadata(ancestor)?;
        let mode = metadata.mode();
        let sticky = mode & 0o1000 != 0;
        if !metadata.is_dir()
            || mode & 0o022 != 0 && !sticky
            || index == 0 && metadata.uid() != effective_user && !sticky
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Unix socket directory chain at `{}` is not protected from namespace replacement",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn unix_socket_marker_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unix socket path must include a file name",
        )
    })?;
    let mut marker_name = file_name.to_os_string();
    marker_name.push(".oxiroute.lock");
    Ok(path.with_file_name(marker_name))
}

pub(super) fn remove_stale_unix_socket(path: &Path) -> io::Result<()> {
    use rustix::{
        io::Errno,
        net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with},
    };
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Ok(());
    }
    let identity = PathIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        path: path.to_owned(),
        socket: true,
    };
    let address = SocketAddrUnix::new(path).map_err(io::Error::from)?;
    let probe = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )
    .map_err(io::Error::from)?;
    match connect(&probe, &address) {
        Err(Errno::CONNREFUSED | Errno::NOENT) => {
            identity.remove_if_unchanged();
        }
        Ok(()) | Err(Errno::AGAIN | Errno::INPROGRESS | Errno::ALREADY) => {}
        Err(source) => return Err(source.into()),
    }
    Ok(())
}
