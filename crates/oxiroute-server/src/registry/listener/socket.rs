use std::{
    fs, io,
    net::SocketAddr,
    os::unix::{
        fs::{MetadataExt as _, PermissionsExt as _},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    process,
};

use tokio::net::{TcpListener, TcpSocket};

/// A bound unix socket path, removed on drop unless a newer process has replaced it.
pub(super) struct SocketFile {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketFile {
    /// Binds under a private name, applies `mode`, then renames over `path`, so clients never see
    /// a socket with the wrong mode and a restarting or handing-over process replaces it atomically.
    pub(super) fn bind(path: &Path, mode: u16) -> io::Result<(UnixListener, Self)> {
        let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let staging = path.with_file_name(format!(".{}.{}", name.to_string_lossy(), process::id()));
        let _ = fs::remove_file(&staging);
        let listener = UnixListener::bind(&staging)?;
        let published = (|| -> io::Result<fs::Metadata> {
            fs::set_permissions(&staging, fs::Permissions::from_mode(u32::from(mode)))?;
            let metadata = fs::symlink_metadata(&staging)?;
            fs::rename(&staging, path)?;
            listener.set_nonblocking(true)?;
            Ok(metadata)
        })()
        .inspect_err(|_| drop(fs::remove_file(&staging)))?;
        let file = Self {
            path: path.to_owned(),
            device: published.dev(),
            inode: published.ino(),
        };
        Ok((listener, file))
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let unchanged = fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode);
        if unchanged {
            drop(fs::remove_file(&self.path));
        }
    }
}

/// Binds a TCP endpoint that a handing-over process may share for the moment both are running.
pub(super) fn bind_tcp(address: SocketAddr) -> io::Result<TcpListener> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.set_reuseport(true)?;
    socket.bind(address)?;
    socket.listen(128)
}
