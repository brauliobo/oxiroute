use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use oxiroute_config::{Config, ListenerBind};

#[derive(Clone)]
pub struct ListenerReservation {
    inner: Arc<ReservationInner>,
}

struct ReservationInner {
    bind: ListenerBind,
    bind_text: String,
    #[cfg(unix)]
    socket: ReservedSocket,
    #[cfg(unix)]
    unix_socket: Option<UnixSocketIdentity>,
}

#[cfg(unix)]
enum ReservedSocket {
    Tcp(std::net::TcpListener),
    Unix(std::os::unix::net::UnixListener),
}

#[cfg(unix)]
struct UnixSocketIdentity {
    path_identity: PathIdentity,
    _lease: UnixSocketLease,
}

#[cfg(unix)]
struct PathIdentity {
    device: u64,
    inode: u64,
    path: PathBuf,
    socket: bool,
}

#[cfg(unix)]
impl PathIdentity {
    fn remove_if_unchanged(&self) {
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

#[cfg(unix)]
struct UnixSocketLease {
    _descriptor: rustix::fd::OwnedFd,
    marker_identity: PathIdentity,
    committed: bool,
}

#[cfg(unix)]
impl UnixSocketLease {
    fn acquire(path: &Path) -> io::Result<Self> {
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
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

#[cfg(unix)]
impl Drop for UnixSocketLease {
    fn drop(&mut self) {
        if !self.committed {
            self.marker_identity.remove_if_unchanged();
        }
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
fn unix_socket_marker_path(path: &Path) -> io::Result<PathBuf> {
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

#[cfg(unix)]
fn remove_stale_unix_socket(path: &Path) -> io::Result<()> {
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

impl Drop for ReservationInner {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(unix_socket) = &self.unix_socket {
            unix_socket.path_identity.remove_if_unchanged();
        }
    }
}

impl ListenerReservation {
    /// Reserves a listener without starting accepts.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be bound securely or the transport is unsupported.
    pub fn bind(listener_name: &str, bind: &ListenerBind) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let (socket, unix_socket, bind_text) = match bind {
                ListenerBind::Socket { address } => {
                    let listener = std::net::TcpListener::bind(address).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind socket `{address}`: {source}"
                            ),
                        )
                    })?;
                    listener.set_nonblocking(true)?;
                    (ReservedSocket::Tcp(listener), None, address.to_string())
                }
                ListenerBind::Udp { address } => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "listener `{listener_name}` cannot reserve UDP socket `{address}` yet"
                        ),
                    ));
                }
                ListenerBind::Unix { path, mode } => {
                    let path_text = path.to_str().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "listener `{listener_name}` Unix socket path is not valid UTF-8 and cannot be bound"
                            ),
                        )
                    })?;
                    let mut lease = UnixSocketLease::acquire(path)?;
                    remove_stale_unix_socket(path)?;
                    let listener = std::os::unix::net::UnixListener::bind(path).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind Unix socket `{path_text}`: {source}"
                            ),
                        )
                    })?;
                    listener.set_nonblocking(true)?;
                    let metadata = std::fs::symlink_metadata(path)?;
                    let path_identity = PathIdentity {
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        path: path.clone(),
                        socket: true,
                    };
                    if let Some(mode) = mode {
                        use std::os::unix::fs::PermissionsExt as _;
                        if let Err(source) = std::fs::set_permissions(
                            path,
                            std::fs::Permissions::from_mode(u32::from(*mode)),
                        ) {
                            path_identity.remove_if_unchanged();
                            return Err(source);
                        }
                    }
                    lease.commit();
                    (
                        ReservedSocket::Unix(listener),
                        Some(UnixSocketIdentity {
                            path_identity,
                            _lease: lease,
                        }),
                        path_text.to_owned(),
                    )
                }
            };
            Ok(Self {
                inner: Arc::new(ReservationInner {
                    bind: bind.clone(),
                    bind_text,
                    socket,
                    unix_socket,
                }),
            })
        }
        #[cfg(not(unix))]
        {
            match bind {
                ListenerBind::Socket { address } => Ok(Self {
                    inner: Arc::new(ReservationInner {
                        bind: bind.clone(),
                        bind_text: address.to_string(),
                    }),
                }),
                ListenerBind::Udp { .. } | ListenerBind::Unix { .. } => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("listener `{listener_name}` uses an unsupported transport"),
                )),
            }
        }
    }

    #[must_use]
    pub fn bind_config(&self) -> &ListenerBind {
        &self.inner.bind
    }

    #[must_use]
    pub fn bind_text(&self) -> &str {
        &self.inner.bind_text
    }

    /// Duplicates the reserved descriptor for one Pingora generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot duplicate the descriptor.
    #[cfg(unix)]
    pub fn duplicate_fds(&self) -> io::Result<pingora::server::Fds> {
        use std::os::fd::IntoRawFd as _;

        let fd = match &self.inner.socket {
            ReservedSocket::Tcp(listener) => listener.try_clone()?.into_raw_fd(),
            ReservedSocket::Unix(listener) => listener.try_clone()?.into_raw_fd(),
        };
        let mut fds = pingora::server::Fds::new();
        fds.add(self.inner.bind_text.clone(), fd);
        Ok(fds)
    }
}

#[derive(Clone, Default)]
pub struct ListenerReservations {
    by_name: HashMap<String, ListenerReservation>,
}

impl ListenerReservations {
    /// Reserves every canonical and management listener, reusing matching process reservations.
    ///
    /// # Errors
    ///
    /// Returns an error without publishing the set when any new reservation fails.
    pub fn prepare(config: &Config, previous: Option<&Self>) -> io::Result<Self> {
        Self::prepare_with_reuse(config, previous, false)
    }

    pub(crate) fn prepare_for_validation(
        config: &Config,
        previous: Option<&Self>,
    ) -> io::Result<Self> {
        Self::prepare_with_reuse(config, previous, true)
    }

    fn prepare_with_reuse(
        config: &Config,
        previous: Option<&Self>,
        reuse_unix_path: bool,
    ) -> io::Result<Self> {
        let mut by_name = HashMap::with_capacity(
            config.listeners.len()
                + usize::from(config.management.is_some())
                + config
                    .stats
                    .as_ref()
                    .map_or(0, |stats| stats.binds.len() + stats.pages.len()),
        );
        for listener in &config.listeners {
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&listener.bind, reuse_unix_path))
                .cloned()
                .map_or_else(
                    || ListenerReservation::bind(&listener.name, &listener.bind),
                    Ok,
                )?;
            by_name.insert(listener.name.clone(), reservation);
        }
        if let Some(management) = &config.management {
            let bind = ListenerBind::Socket {
                address: management.bind,
            };
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&bind, reuse_unix_path))
                .cloned()
                .map_or_else(|| ListenerReservation::bind("management", &bind), Ok)?;
            by_name.insert("@management".into(), reservation);
        }
        if let Some(stats) = &config.stats {
            for (index, address) in stats.binds.iter().enumerate() {
                let name = format!("@stats-{index}");
                let bind = ListenerBind::Socket { address: *address };
                let reservation = previous
                    .and_then(|reservations| reservations.by_bind(&bind, reuse_unix_path))
                    .cloned()
                    .map_or_else(|| ListenerReservation::bind(&name, &bind), Ok)?;
                by_name.insert(name, reservation);
            }
            for (index, page) in stats.pages.iter().enumerate() {
                let name = format!("@stats-page-{index}");
                let bind = ListenerBind::Socket { address: page.bind };
                let reservation = previous
                    .and_then(|reservations| reservations.by_bind(&bind, reuse_unix_path))
                    .cloned()
                    .map_or_else(|| ListenerReservation::bind(&name, &bind), Ok)?;
                by_name.insert(name, reservation);
            }
        }
        Ok(Self { by_name })
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ListenerReservation> {
        self.by_name.get(name)
    }

    fn by_bind(&self, bind: &ListenerBind, reuse_unix_path: bool) -> Option<&ListenerReservation> {
        self.by_name.values().find(|reservation| {
            same_bind_identity(reservation.bind_config(), bind)
                || reuse_unix_path && same_unix_path(reservation.bind_config(), bind)
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

fn same_bind_identity(left: &ListenerBind, right: &ListenerBind) -> bool {
    match (left, right) {
        (ListenerBind::Socket { address: left }, ListenerBind::Socket { address: right })
        | (ListenerBind::Udp { address: left }, ListenerBind::Udp { address: right }) => {
            left == right
        }
        (
            ListenerBind::Unix {
                path: left_path,
                mode: left_mode,
            },
            ListenerBind::Unix {
                path: right_path,
                mode: right_mode,
            },
        ) => left_path == right_path && left_mode == right_mode,
        _ => false,
    }
}

fn same_unix_path(left: &ListenerBind, right: &ListenerBind) -> bool {
    matches!(
        (left, right),
        (
            ListenerBind::Unix { path: left, .. },
            ListenerBind::Unix { path: right, .. }
        ) if left == right
    )
}

pub(crate) fn unix_listener_mode_change_requires_restart(
    active: &Config,
    candidate: &Config,
) -> bool {
    candidate.listeners.iter().any(|candidate_listener| {
        let ListenerBind::Unix {
            path: candidate_path,
            mode: candidate_mode,
        } = &candidate_listener.bind
        else {
            return false;
        };
        active.listeners.iter().any(|active_listener| {
            matches!(
                &active_listener.bind,
                ListenerBind::Unix { path, mode }
                    if path == candidate_path && mode != candidate_mode
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use oxiroute_config::Config;

    use super::*;

    fn config(name: &str, address: SocketAddr) -> Config {
        Config {
            listeners: vec![oxiroute_config::Listener {
                name: name.into(),
                bind: ListenerBind::Socket { address },
                protocol: oxiroute_config::Protocol::Rtmp,
                service: Some("rtmp".into()),
                tls_profile: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            ..empty_config()
        }
    }

    fn empty_config() -> Config {
        Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        }
    }

    #[test]
    fn matching_listener_reservations_are_reused_without_rebinding() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first = ListenerReservations::prepare(&config("edge", address), None)
            .expect("first reservation");
        let second = ListenerReservations::prepare(&config("edge", address), Some(&first))
            .expect("reused reservation");

        assert!(Arc::ptr_eq(
            &first.get("edge").expect("first").inner,
            &second.get("edge").expect("second").inner
        ));
    }

    #[test]
    fn listener_reservations_are_reused_after_rename() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first = ListenerReservations::prepare(&config("old-name", address), None)
            .expect("first reservation");
        let second = ListenerReservations::prepare(&config("new-name", address), Some(&first))
            .expect("renamed reservation");

        assert!(Arc::ptr_eq(
            &first.get("old-name").expect("first").inner,
            &second.get("new-name").expect("second").inner
        ));
    }

    #[test]
    fn statistics_reservations_are_reused_after_reorder() {
        let first_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("first temporary bind")
            .local_addr()
            .expect("first address");
        let second_address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("second temporary bind")
            .local_addr()
            .expect("second address");
        let mut first_config = empty_config();
        first_config.stats = Some(oxiroute_config::Stats {
            binds: vec![first_address, second_address],
            admin_token_file: None,
            pages: Vec::new(),
        });
        let first = ListenerReservations::prepare(&first_config, None).expect("first reservations");
        let mut second_config = first_config;
        second_config.stats.as_mut().expect("stats").binds.reverse();
        let second = ListenerReservations::prepare(&second_config, Some(&first))
            .expect("reordered reservations");

        assert!(Arc::ptr_eq(
            &first.get("@stats-0").expect("first original").inner,
            &second.get("@stats-1").expect("first reordered").inner,
        ));
        assert!(Arc::ptr_eq(
            &first.get("@stats-1").expect("second original").inner,
            &second.get("@stats-0").expect("second reordered").inner,
        ));
    }

    #[test]
    fn statistics_page_reservations_are_named_and_reused_by_bind() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let mut config = empty_config();
        config.stats = Some(oxiroute_config::Stats {
            binds: Vec::new(),
            admin_token_file: None,
            pages: vec![oxiroute_config::StatsPage {
                bind: address,
                uri_prefix: "/stats".into(),
                refresh_ms: 1_000,
                admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
        });

        let first = ListenerReservations::prepare(&config, None).expect("first reservations");
        let second =
            ListenerReservations::prepare(&config, Some(&first)).expect("reused page reservation");

        assert_eq!(first.len(), 1);
        assert!(Arc::ptr_eq(
            &first.get("@stats-page-0").expect("first page").inner,
            &second.get("@stats-page-0").expect("second page").inner,
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_reservation_reload_reuses_only_an_identical_path_and_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("Unix reservation directory");
        let path = directory.path().join("reload.sock");
        let mut first_config = empty_config();
        first_config.listeners.push(oxiroute_config::Listener {
            name: "unix".into(),
            bind: ListenerBind::Unix {
                path: path.clone(),
                mode: Some(0o600),
            },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let first = ListenerReservations::prepare(&first_config, None).expect("first Unix bind");
        let unchanged = ListenerReservations::prepare(&first_config, Some(&first))
            .expect("identical Unix bind reuse");
        assert!(Arc::ptr_eq(
            &first.get("unix").expect("first").inner,
            &unchanged.get("unix").expect("unchanged").inner,
        ));

        let mut changed_config = first_config.clone();
        changed_config.listeners[0].bind = ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o660),
        };
        let validated = ListenerReservations::prepare_for_validation(&changed_config, Some(&first))
            .expect("validation reuses the active Unix path");
        assert!(Arc::ptr_eq(
            &first.get("unix").expect("first").inner,
            &validated.get("unix").expect("validated").inner,
        ));
        assert!(unix_listener_mode_change_requires_restart(
            &first_config,
            &changed_config,
        ));
        changed_config.listeners[0].max_connections = Some(7);
        assert!(unix_listener_mode_change_requires_restart(
            &first_config,
            &changed_config,
        ));
        assert!(ListenerReservations::prepare(&changed_config, Some(&first)).is_err());
        assert_eq!(
            std::fs::metadata(path)
                .expect("reserved socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "a changed mode must not silently reuse the old reservation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_reservation_reclaims_only_a_stale_socket() {
        let directory = tempfile::tempdir().expect("Unix reservation directory");
        let stale_path = directory.path().join("stale.sock");
        std::os::unix::net::UnixListener::bind(&stale_path).expect("stale Unix bind");

        let reservation = ListenerReservation::bind(
            "stale",
            &ListenerBind::Unix {
                path: stale_path.clone(),
                mode: Some(0o600),
            },
        )
        .expect("stale socket reclaimed");
        assert_eq!(
            reservation.bind_text(),
            stale_path.to_str().expect("UTF-8 path")
        );
        let concurrent = ListenerReservation::bind(
            "concurrent",
            &ListenerBind::Unix {
                path: stale_path.clone(),
                mode: Some(0o600),
            },
        )
        .err()
        .expect("the active reservation owns the Unix namespace");
        assert_eq!(concurrent.kind(), io::ErrorKind::AddrInUse);

        let active_path = directory.path().join("active.sock");
        let active =
            std::os::unix::net::UnixListener::bind(&active_path).expect("active Unix bind");
        assert!(
            ListenerReservation::bind(
                "active",
                &ListenerBind::Unix {
                    path: active_path.clone(),
                    mode: None,
                },
            )
            .is_err()
        );
        assert!(active_path.exists());
        drop(active);
    }

    #[cfg(unix)]
    #[test]
    fn restrictive_stale_socket_is_preserved_when_liveness_cannot_be_proven() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("Unix reservation directory");
        let path = directory.path().join("restrictive.sock");
        let mut lease = UnixSocketLease::acquire(&path).expect("Unix namespace lease");
        lease.commit();
        drop(lease);

        let stale = std::os::unix::net::UnixListener::bind(&path).expect("stale Unix bind");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("restrict stale Unix socket");
        drop(stale);

        let error = ListenerReservation::bind(
            "restrictive",
            &ListenerBind::Unix {
                path: path.clone(),
                mode: Some(0o400),
            },
        )
        .err()
        .expect("permission-denied stale socket must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_reservation_rejects_an_unprotected_writable_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("Unix reservation directory");
        let unsafe_directory = directory.path().join("shared");
        std::fs::create_dir(&unsafe_directory).expect("shared Unix socket directory");
        std::fs::set_permissions(&unsafe_directory, std::fs::Permissions::from_mode(0o770))
            .expect("writable shared directory");
        let path = unsafe_directory.join("listener.sock");

        let error = ListenerReservation::bind(
            "unsafe",
            &ListenerBind::Unix {
                path,
                mode: Some(0o660),
            },
        )
        .err()
        .expect("non-sticky writable parent must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let protected_directory = unsafe_directory.join("private");
        std::fs::create_dir(&protected_directory).expect("private Unix socket directory");
        std::fs::set_permissions(&protected_directory, std::fs::Permissions::from_mode(0o700))
            .expect("private socket directory");
        let error = ListenerReservation::bind(
            "unsafe-ancestor",
            &ListenerBind::Unix {
                path: protected_directory.join("listener.sock"),
                mode: Some(0o600),
            },
        )
        .err()
        .expect("writable ancestor must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
