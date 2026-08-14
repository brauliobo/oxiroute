use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use oxiroute_config::{ListenerBind, ValidatedConfig};
#[cfg(target_os = "linux")]
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSet,
    DescriptorSlot, MAX_DESCRIPTOR_COUNT, SlotId,
};
#[cfg(target_os = "linux")]
use oxiroute_supervisor_master::StableListeners;

use crate::listener_inventory::{
    ListenerDescriptorKind, ListenerDescriptorRole, ListenerId, ListenerInventory,
};

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
    Udp(std::net::UdpSocket),
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
        })
    }
}

#[cfg(unix)]
impl Drop for UnixSocketLease {
    fn drop(&mut self) {
        self.marker_identity.remove_if_unchanged();
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
    #[allow(clippy::too_many_lines)]
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
                    let socket = std::net::UdpSocket::bind(address).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind UDP socket `{address}`: {source}"
                            ),
                        )
                    })?;
                    socket.set_nonblocking(true)?;
                    (
                        ReservedSocket::Udp(socket),
                        None,
                        format!("udp://{address}"),
                    )
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
                    let lease = UnixSocketLease::acquire(path)?;
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

        let fd = self.duplicate_owned_fd()?.into_raw_fd();
        let mut fds = pingora::server::Fds::new();
        fds.add(self.inner.bind_text.clone(), fd);
        Ok(fds)
    }

    /// Creates one independently owned, close-on-exec duplicate.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot duplicate the listener descriptor.
    #[cfg(unix)]
    pub fn duplicate_owned_fd(&self) -> io::Result<std::os::fd::OwnedFd> {
        let descriptor = match &self.inner.socket {
            ReservedSocket::Tcp(listener) => rustix::io::fcntl_dupfd_cloexec(listener, 0),
            ReservedSocket::Udp(socket) => rustix::io::fcntl_dupfd_cloexec(socket, 0),
            ReservedSocket::Unix(listener) => rustix::io::fcntl_dupfd_cloexec(listener, 0),
        };
        descriptor.map_err(io::Error::from)
    }

    /// Duplicates an owned UDP socket for a standalone datagram runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when this reservation is not a UDP socket or the operating system cannot
    /// duplicate its descriptor.
    #[cfg(unix)]
    pub fn duplicate_udp_socket(&self) -> io::Result<std::net::UdpSocket> {
        if !matches!(&self.inner.socket, ReservedSocket::Udp(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "listener reservation is not a UDP socket",
            ));
        }
        Ok(std::net::UdpSocket::from(self.duplicate_owned_fd()?))
    }

    #[cfg(not(unix))]
    pub fn duplicate_udp_socket(&self) -> io::Result<std::net::UdpSocket> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "UDP listener reservations are unsupported on this platform",
        ))
    }

    #[cfg(target_os = "linux")]
    fn adopt(bind: ListenerBind, descriptor: std::os::fd::OwnedFd) -> Self {
        let bind_text = match &bind {
            ListenerBind::Socket { address } => address.to_string(),
            ListenerBind::Udp { address } => format!("udp://{address}"),
            ListenerBind::Unix { path, .. } => path.to_string_lossy().into_owned(),
        };
        let socket = match &bind {
            ListenerBind::Socket { .. } => ReservedSocket::Tcp(descriptor.into()),
            ListenerBind::Udp { .. } => ReservedSocket::Udp(descriptor.into()),
            ListenerBind::Unix { .. } => ReservedSocket::Unix(descriptor.into()),
        };
        Self {
            inner: Arc::new(ReservationInner {
                bind,
                bind_text,
                socket,
                unix_socket: None,
            }),
        }
    }
}

type ReservationKey = ListenerId;

#[derive(Clone, Default)]
pub struct ListenerReservations {
    by_key: HashMap<ReservationKey, ListenerReservation>,
}

impl ListenerReservations {
    /// Reserves every canonical and management listener, reusing matching process reservations.
    ///
    /// # Errors
    ///
    /// Returns an error without publishing the set when any new reservation fails.
    ///
    /// ```compile_fail
    /// use oxiroute_config::ConfigDraft;
    /// use oxiroute_server::ListenerReservations;
    ///
    /// let draft: ConfigDraft = todo!();
    /// let _ = ListenerReservations::prepare(&draft, None);
    /// ```
    pub fn prepare(config: &ValidatedConfig, previous: Option<&Self>) -> io::Result<Self> {
        Self::prepare_inventory(&ListenerInventory::compile(config), previous, false)
    }

    pub(crate) fn prepare_for_validation(
        config: &ValidatedConfig,
        previous: Option<&Self>,
    ) -> io::Result<Self> {
        // Validation intentionally performs real, temporary bind probes. Dropping the returned
        // set closes descriptors and removes any Unix socket namespace entries it created.
        Self::prepare_inventory(&ListenerInventory::compile(config), previous, true)
    }

    fn prepare_inventory(
        inventory: &ListenerInventory,
        previous: Option<&Self>,
        reuse_unix_path: bool,
    ) -> io::Result<Self> {
        let mut by_key = HashMap::with_capacity(inventory.len());
        for entry in inventory.entries() {
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&entry.bind, reuse_unix_path))
                .cloned()
                .map_or_else(|| ListenerReservation::bind(&entry.name, &entry.bind), Ok)?;
            by_key.insert(entry.id.clone(), reservation);
        }
        Ok(Self { by_key })
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ListenerReservation> {
        self.by_key
            .get(&ReservationKey::Traffic(name.to_owned()))
            .or_else(|| legacy_reservation_key(name).and_then(|key| self.by_key.get(&key)))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn management(&self) -> Option<&ListenerReservation> {
        self.by_key.get(&ReservationKey::Management)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn stats(&self, index: usize) -> Option<&ListenerReservation> {
        self.by_key.get(&ReservationKey::Stats(index))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn stats_page(&self, index: usize) -> Option<&ListenerReservation> {
        self.by_key.get(&ReservationKey::StatsPage(index))
    }

    /// Consumes these reservations into stable master listeners while retaining Unix namespace
    /// ownership for the complete master lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when the config exceeds the worker descriptor limit, does not match this
    /// reservation set, or listener ownership cannot be established.
    ///
    /// ```compile_fail
    /// use oxiroute_config::ConfigDraft;
    /// use oxiroute_server::ListenerReservations;
    ///
    /// let draft: ConfigDraft = todo!();
    /// let reservations: ListenerReservations = todo!();
    /// let _ = reservations.into_stable_listeners(&draft);
    /// ```
    #[cfg(target_os = "linux")]
    pub fn into_stable_listeners(self, config: &ValidatedConfig) -> io::Result<StableListeners> {
        let owner = Arc::new(self);
        let (manifest, originals) =
            owner.export_descriptors(&ListenerInventory::compile(config))?;
        StableListeners::new(manifest, originals, Arc::clone(&owner)).map_err(io::Error::other)
    }

    #[cfg(target_os = "linux")]
    fn export_descriptors(
        &self,
        inventory: &ListenerInventory,
    ) -> io::Result<(DescriptorManifest, Vec<std::os::fd::OwnedFd>)> {
        let slots = descriptor_slots(inventory)?;
        preflight_descriptor_capacity(inventory)?;
        if self.by_key.len() != inventory.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "reservation set contains {} entries, but config requires {}",
                    self.by_key.len(),
                    inventory.len()
                ),
            ));
        }
        let mut ordered = Vec::with_capacity(inventory.len());
        for (entry, slot) in inventory.entries().iter().zip(&slots) {
            let reservation = self.by_key.get(&entry.id).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("reservation set is missing {:?}", entry.id),
                )
            })?;
            let expected_bind = listener_bind_from_slot(slot)?;
            if !same_bind_identity(reservation.bind_config(), &expected_bind) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "reservation {:?} bind or mode does not match config",
                        entry.id
                    ),
                ));
            }
            ordered.push(reservation);
        }
        let manifest = DescriptorManifest::new(slots).map_err(io::Error::other)?;
        let originals = ordered
            .into_iter()
            .map(ListenerReservation::duplicate_owned_fd)
            .collect::<io::Result<Vec<_>>>()?;
        Ok((manifest, originals))
    }

    /// Converts one exact manifest-bound worker descriptor set into listener reservations.
    ///
    /// Adopted Unix reservations intentionally carry no namespace lease or unlink ownership.
    ///
    /// # Errors
    ///
    /// Returns an error for descriptor limits, cardinality, slot, role, kind, bind, mode, or
    /// consume-once mismatches.
    #[cfg(target_os = "linux")]
    pub(crate) fn adopt(config: &ValidatedConfig, descriptors: DescriptorSet) -> io::Result<Self> {
        Self::adopt_inventory(&ListenerInventory::compile(config), descriptors)
    }

    #[cfg(target_os = "linux")]
    fn adopt_inventory(
        inventory: &ListenerInventory,
        mut descriptors: DescriptorSet,
    ) -> io::Result<Self> {
        let slots = descriptor_slots(inventory)?;
        preflight_descriptor_capacity(inventory)?;
        if descriptors.remaining() != inventory.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "worker config expects {} listener descriptors, but adoption contains {}",
                    inventory.len(),
                    descriptors.remaining()
                ),
            ));
        }
        for expected in &slots {
            if descriptors.slot(expected.id) != Some(expected) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "listener descriptor slot {:?} does not match config",
                        expected.id
                    ),
                ));
            }
        }

        let mut by_key = HashMap::with_capacity(inventory.len());
        for (entry, slot) in inventory.entries().iter().zip(slots) {
            let descriptor = descriptors.take(slot.id).map_err(io::Error::other)?;
            let bind = listener_bind_from_slot(&slot)?;
            by_key.insert(
                entry.id.clone(),
                ListenerReservation::adopt(bind, descriptor),
            );
        }
        if descriptors.remaining() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "listener adoption left unconsumed descriptor slots",
            ));
        }
        Ok(Self { by_key })
    }

    fn by_bind(&self, bind: &ListenerBind, reuse_unix_path: bool) -> Option<&ListenerReservation> {
        self.by_key.values().find(|reservation| {
            same_bind_identity(reservation.bind_config(), bind)
                || reuse_unix_path && same_unix_path(reservation.bind_config(), bind)
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    #[doc(hidden)]
    #[must_use]
    pub fn listener_restart_required(
        mode: crate::RuntimeMode,
        active: &ValidatedConfig,
        candidate: &ValidatedConfig,
    ) -> bool {
        ListenerInventory::compile(active)
            .restart_required(mode, &ListenerInventory::compile(candidate))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn same_supervised_listener_topology(
        active: &ValidatedConfig,
        candidate: &ValidatedConfig,
    ) -> bool {
        !Self::listener_restart_required(crate::RuntimeMode::Supervised, active, candidate)
    }

    #[doc(hidden)]
    pub fn validate_supervised_descriptor_limit(
        config: &ValidatedConfig,
    ) -> Result<(), &'static str> {
        if ListenerInventory::compile(config).len() > MAX_DESCRIPTOR_COUNT {
            return Err("Stage 2 worker listener descriptor limit is 64");
        }
        Ok(())
    }
}

fn legacy_reservation_key(name: &str) -> Option<ReservationKey> {
    if name == "@management" {
        return Some(ReservationKey::Management);
    }
    if let Some(index) = name.strip_prefix("@stats-page-") {
        let index = index.parse::<usize>().ok()?;
        return (name == format!("@stats-page-{index}"))
            .then_some(ReservationKey::StatsPage(index));
    }
    if let Some(index) = name.strip_prefix("@stats-") {
        let index = index.parse::<usize>().ok()?;
        return (name == format!("@stats-{index}")).then_some(ReservationKey::Stats(index));
    }
    None
}

#[cfg(target_os = "linux")]
fn descriptor_slots(inventory: &ListenerInventory) -> io::Result<Vec<DescriptorSlot>> {
    if inventory.len() > MAX_DESCRIPTOR_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worker configuration has {} listener descriptors; maximum is {MAX_DESCRIPTOR_COUNT}",
                inventory.len()
            ),
        ));
    }

    Ok(inventory
        .entries()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            descriptor_slot(
                SlotId(u16::try_from(index).expect("descriptor limit checked")),
                descriptor_role(&entry.descriptor_role),
                entry.descriptor_kind,
                &entry.bind,
            )
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn descriptor_role(role: &ListenerDescriptorRole) -> DescriptorRole {
    match role {
        ListenerDescriptorRole::Traffic(name) => DescriptorRole::Traffic(name.clone()),
        ListenerDescriptorRole::Management => DescriptorRole::Management,
        ListenerDescriptorRole::Stats(index) => {
            DescriptorRole::Stats(u16::try_from(*index).expect("descriptor limit checked"))
        }
        ListenerDescriptorRole::StatsPage(index) => {
            DescriptorRole::StatsPage(u16::try_from(*index).expect("descriptor limit checked"))
        }
    }
}

#[cfg(target_os = "linux")]
const DESCRIPTOR_HEADROOM: u64 = 32;

#[cfg(target_os = "linux")]
fn descriptor_capacity_required(inventory: &ListenerInventory, current_open: u64) -> u64 {
    let listeners = u64::try_from(inventory.len()).unwrap_or(u64::MAX);
    let unix_leases = u64::try_from(
        inventory
            .entries()
            .iter()
            .filter(|entry| entry.descriptor_kind == ListenerDescriptorKind::Unix)
            .count(),
    )
    .unwrap_or(u64::MAX);
    current_open
        .saturating_add(DESCRIPTOR_HEADROOM)
        .saturating_add(listeners.saturating_mul(3))
        .saturating_add(unix_leases)
}

#[cfg(target_os = "linux")]
fn current_open_descriptor_count() -> io::Result<u64> {
    let entries = std::fs::read_dir("/proc/self/fd")?;
    let mut count = 0_u64;
    for entry in entries {
        entry?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

#[cfg(target_os = "linux")]
fn validate_descriptor_capacity(
    inventory: &ListenerInventory,
    current_open: u64,
    soft_limit: Option<u64>,
) -> io::Result<()> {
    let required = descriptor_capacity_required(inventory, current_open);
    if soft_limit.is_none_or(|available| available >= required) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "listener descriptor transfer requires capacity for at least {required} open descriptors ({current_open} currently open), but the RLIMIT_NOFILE soft limit is {}",
        soft_limit.expect("finite limit checked above")
    )))
}

#[cfg(target_os = "linux")]
fn preflight_descriptor_capacity(inventory: &ListenerInventory) -> io::Result<()> {
    let current_open = current_open_descriptor_count()?;
    validate_descriptor_capacity(
        inventory,
        current_open,
        rustix::process::getrlimit(rustix::process::Resource::Nofile).current,
    )
}

#[cfg(target_os = "linux")]
fn descriptor_slot(
    id: SlotId,
    role: DescriptorRole,
    descriptor_kind: ListenerDescriptorKind,
    bind: &ListenerBind,
) -> DescriptorSlot {
    let kind = match descriptor_kind {
        ListenerDescriptorKind::Tcp => DescriptorKind::TcpListener,
        ListenerDescriptorKind::Unix => DescriptorKind::UnixListener,
        ListenerDescriptorKind::Datagram => DescriptorKind::DatagramListener,
        ListenerDescriptorKind::Quic => DescriptorKind::QuicListener,
    };
    let (bind, mode) = match bind {
        ListenerBind::Socket { address } | ListenerBind::Udp { address } => {
            (Some(BindIdentity::Tcp(*address)), None)
        }
        ListenerBind::Unix { path, mode } => (Some(BindIdentity::UnixPath(path.clone())), *mode),
    };
    DescriptorSlot {
        id,
        role,
        kind,
        bind,
        mode,
    }
}

#[cfg(target_os = "linux")]
fn listener_bind_from_slot(slot: &DescriptorSlot) -> io::Result<ListenerBind> {
    match (&slot.kind, &slot.bind) {
        (DescriptorKind::TcpListener, Some(BindIdentity::Tcp(address))) => {
            Ok(ListenerBind::Socket { address: *address })
        }
        (DescriptorKind::UnixListener, Some(BindIdentity::UnixPath(path))) => {
            Ok(ListenerBind::Unix {
                path: path.clone(),
                mode: slot.mode,
            })
        }
        (
            DescriptorKind::DatagramListener | DescriptorKind::QuicListener,
            Some(BindIdentity::Tcp(address)),
        ) => Ok(ListenerBind::Udp { address: *address }),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "listener descriptor slot {:?} has no supported bind",
                slot.id
            ),
        )),
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

#[cfg(test)]
mod tests {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        path::PathBuf,
    };

    use oxiroute_config::{ConfigDraft, Protocol, ValidatedConfig};
    use serde_json::json;

    use super::*;
    use crate::listener_inventory::{
        ListenerDescriptorKind, ListenerDescriptorRole, ListenerId, ListenerInventory,
        ListenerMetricPolicy, ListenerPlane,
    };

    fn config(name: &str, address: SocketAddr) -> ConfigDraft {
        ConfigDraft {
            listeners: vec![oxiroute_config::Listener {
                name: name.into(),
                bind: ListenerBind::Socket { address },
                protocol: oxiroute_config::Protocol::Rtmp,
                service: Some("rtmp".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
            ..empty_config()
        }
    }

    fn empty_config() -> ConfigDraft {
        ConfigDraft {
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

    #[expect(
        clippy::too_many_lines,
        reason = "one fixture keeps the complete listener inventory order visible"
    )]
    fn differential_config() -> ValidatedConfig {
        serde_json::from_value::<ConfigDraft>(json!({
            "version": 1,
            "management": { "bind": "127.0.0.1:9900" },
            "stats": {
                "binds": ["127.0.0.1:8404", "127.0.0.1:8405"],
                "pages": [{
                    "bind": "127.0.0.1:8406",
                    "uri_prefix": "/stats",
                    "refresh_ms": 1000,
                    "admin": "disabled",
                    "max_connections": 17
                }]
            },
            "certificates": [{
                "name": "downstream",
                "dns_names": ["proxy.example.test"],
                "source": {
                    "type": "files",
                    "certificate_chain_path": "/tmp/oxiroute-listener-inventory-chain.pem",
                    "private_key_path": "/tmp/oxiroute-listener-inventory-key.pem"
                }
            }],
            "tls_profiles": [{
                "name": "h3",
                "certificates": ["downstream"],
                "default_certificate": "downstream",
                "min_version": "1.3",
                "alpn": ["h3"]
            }],
            "listeners": [
                {
                    "name": "http",
                    "bind": { "type": "socket", "address": "127.0.0.1:7996" },
                    "protocol": "http",
                    "service": "web",
                    "max_connections": 7
                },
                {
                    "name": "rtmp",
                    "bind": { "type": "socket", "address": "127.0.0.1:7997" },
                    "protocol": "rtmp",
                    "service": "live",
                    "max_connections": 8
                },
                {
                    "name": "forward-h1",
                    "bind": { "type": "socket", "address": "127.0.0.1:7998" },
                    "protocol": "forward_http1",
                    "service": "forward",
                    "max_connections": 9
                },
                {
                    "name": "forward-h2",
                    "bind": { "type": "socket", "address": "127.0.0.1:7999" },
                    "protocol": "forward_http2",
                    "service": "forward",
                    "max_connections": 10
                },
                {
                    "name": "tcp",
                    "bind": { "type": "socket", "address": "127.0.0.1:8000" },
                    "protocol": "tcp",
                    "service": "relay",
                    "max_connections": 11
                },
                {
                    "name": "unix",
                    "bind": { "type": "unix", "path": "/tmp/inventory.sock", "mode": 384 },
                    "protocol": "tcp",
                    "service": "relay"
                },
                {
                    "name": "udp",
                    "bind": { "type": "udp", "address": "127.0.0.1:8001" },
                    "protocol": "udp",
                    "service": "relay"
                },
                {
                    "name": "h3",
                    "bind": { "type": "udp", "address": "127.0.0.1:8002" },
                    "protocol": "http3",
                    "service": "web",
                    "tls_profile": "h3"
                },
                {
                    "name": "forward-h3",
                    "bind": { "type": "udp", "address": "127.0.0.1:8003" },
                    "protocol": "forward_http3",
                    "service": "forward",
                    "tls_profile": "h3"
                }
            ],
            "http_services": [{
                "name": "web",
                "routes": [{
                    "path": { "kind": "segment_prefix", "value": "/" },
                    "policy": { "request_buffering": true },
                    "action": { "type": "fixed_response", "status": 200, "body": "ok" }
                }],
                "max_request_body_bytes": 65536
            }],
            "forward_proxy_services": [{
                "name": "forward",
                "enabled_versions": ["h1", "h2", "h3"],
                "tls_required": false
            }],
            "rtmp_services": [{
                "name": "live",
                "applications": [{ "name": "broadcast", "live": true }]
            }],
            "upstream_pools": [{
                "name": "origin",
                "endpoints": [{ "type": "socket", "address": "127.0.0.1:9000" }],
                "algorithm": "round_robin"
            }],
            "l4_services": [{
                "name": "relay",
                "upstream_pool": "origin",
                "udp": {}
            }]
        }))
        .expect("inventory draft")
        .validate()
        .expect("valid inventory config")
    }

    fn many_listener_config(count: usize) -> ValidatedConfig {
        let mut config = empty_config();
        config.upstream_pools.push(oxiroute_config::UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![oxiroute_config::UpstreamEndpoint::Socket {
                address: "127.0.0.1:9000".parse().expect("upstream address"),
            }],
            algorithm: oxiroute_config::UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: oxiroute_config::HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        });
        config.l4_services.push(oxiroute_config::L4Service {
            name: "relay".into(),
            upstream_pool: "origin".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 1_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        });
        config.listeners = (0..count)
            .map(|index| oxiroute_config::Listener {
                name: format!("listener-{index}"),
                bind: ListenerBind::Socket {
                    address: SocketAddr::from((
                        [127, 0, 0, 1],
                        10_000 + u16::try_from(index).expect("bounded listener index"),
                    )),
                },
                protocol: Protocol::Tcp,
                service: Some("relay".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            })
            .collect();
        config.validate().expect("valid many-listener config")
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, PartialEq)]
    struct ExpectedInventoryEntry {
        id: ListenerId,
        name: &'static str,
        protocol: Protocol,
        bind: ListenerBind,
        descriptor_role: ListenerDescriptorRole,
        descriptor_kind: ListenerDescriptorKind,
        descriptor_protocol: Option<Protocol>,
        policy: (ListenerPlane, ListenerMetricPolicy, Option<u64>),
    }

    #[cfg(target_os = "linux")]
    fn assert_inventory_and_slots(
        config: &ValidatedConfig,
        expected_inventory: &[ExpectedInventoryEntry],
        expected_slots: &[DescriptorSlot],
    ) {
        let inventory = ListenerInventory::compile(config);
        let slots = descriptor_slots(&inventory).expect("descriptor slots");
        assert_eq!(slots, expected_slots);
        assert_eq!(inventory.entries().len(), expected_inventory.len());
        for (actual, expected) in inventory.entries().iter().zip(expected_inventory) {
            assert_eq!(actual.id, expected.id);
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.protocol, expected.protocol);
            assert_eq!(actual.bind, expected.bind);
            assert_eq!(actual.descriptor_role, expected.descriptor_role);
            assert_eq!(actual.descriptor_kind, expected.descriptor_kind);
            assert_eq!(actual.descriptor_protocol(), expected.descriptor_protocol);
            assert_eq!(
                (actual.plane, actual.metric_policy, actual.max_connections),
                expected.policy
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn expected_traffic(
        name: &'static str,
        port: u16,
        protocol: Protocol,
        descriptor_kind: ListenerDescriptorKind,
        max_connections: Option<u64>,
    ) -> ExpectedInventoryEntry {
        ExpectedInventoryEntry {
            id: ListenerId::Traffic(name.into()),
            name,
            protocol,
            bind: ListenerBind::Socket {
                address: SocketAddr::from(([127, 0, 0, 1], port)),
            },
            descriptor_role: ListenerDescriptorRole::Traffic(name.into()),
            descriptor_kind,
            descriptor_protocol: Some(protocol),
            policy: (
                ListenerPlane::Data,
                ListenerMetricPolicy::Public,
                max_connections,
            ),
        }
    }

    #[cfg(target_os = "linux")]
    fn expected_udp_traffic(
        name: &'static str,
        port: u16,
        protocol: Protocol,
        descriptor_kind: ListenerDescriptorKind,
    ) -> ExpectedInventoryEntry {
        ExpectedInventoryEntry {
            id: ListenerId::Traffic(name.into()),
            name,
            protocol,
            bind: ListenerBind::Udp {
                address: SocketAddr::from(([127, 0, 0, 1], port)),
            },
            descriptor_role: ListenerDescriptorRole::Traffic(name.into()),
            descriptor_kind,
            descriptor_protocol: Some(protocol),
            policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
        }
    }

    #[cfg(target_os = "linux")]
    fn expected_stats(index: usize, port: u16) -> ExpectedInventoryEntry {
        ExpectedInventoryEntry {
            id: ListenerId::Stats(index),
            name: if index == 0 { "@stats-0" } else { "@stats-1" },
            protocol: Protocol::Http,
            bind: ListenerBind::Socket {
                address: SocketAddr::from(([127, 0, 0, 1], port)),
            },
            descriptor_role: ListenerDescriptorRole::Stats(index),
            descriptor_kind: ListenerDescriptorKind::Tcp,
            descriptor_protocol: None,
            policy: (
                ListenerPlane::Control,
                ListenerMetricPolicy::InternalOnly,
                None,
            ),
        }
    }

    #[cfg(target_os = "linux")]
    fn expected_tcp_slot(id: u16, role: DescriptorRole, port: u16) -> DescriptorSlot {
        DescriptorSlot {
            id: SlotId(id),
            role,
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(SocketAddr::from(([127, 0, 0, 1], port)))),
            mode: None,
        }
    }

    #[cfg(target_os = "linux")]
    fn expected_datagram_slot(
        id: u16,
        name: &str,
        port: u16,
        kind: DescriptorKind,
    ) -> DescriptorSlot {
        DescriptorSlot {
            id: SlotId(id),
            role: DescriptorRole::Traffic(name.into()),
            kind,
            bind: Some(BindIdentity::Tcp(SocketAddr::from(([127, 0, 0, 1], port)))),
            mode: None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one differential assertion pins every inventory field in canonical order"
    )]
    fn inventory_and_descriptor_slots_match_independent_mixed_expectations() {
        let config = differential_config();
        let expected_inventory = vec![
            expected_traffic(
                "http",
                7_996,
                Protocol::Http,
                ListenerDescriptorKind::Tcp,
                Some(7),
            ),
            expected_traffic(
                "rtmp",
                7_997,
                Protocol::Rtmp,
                ListenerDescriptorKind::Tcp,
                Some(8),
            ),
            expected_traffic(
                "forward-h1",
                7_998,
                Protocol::ForwardHttp1,
                ListenerDescriptorKind::Tcp,
                Some(9),
            ),
            expected_traffic(
                "forward-h2",
                7_999,
                Protocol::ForwardHttp2,
                ListenerDescriptorKind::Tcp,
                Some(10),
            ),
            expected_traffic(
                "tcp",
                8_000,
                Protocol::Tcp,
                ListenerDescriptorKind::Tcp,
                Some(11),
            ),
            ExpectedInventoryEntry {
                id: ListenerId::Traffic("unix".into()),
                name: "unix",
                protocol: Protocol::Tcp,
                bind: ListenerBind::Unix {
                    path: PathBuf::from("/tmp/inventory.sock"),
                    mode: Some(0o600),
                },
                descriptor_role: ListenerDescriptorRole::Traffic("unix".into()),
                descriptor_kind: ListenerDescriptorKind::Unix,
                descriptor_protocol: Some(Protocol::Tcp),
                policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
            },
            expected_udp_traffic(
                "udp",
                8_001,
                Protocol::Udp,
                ListenerDescriptorKind::Datagram,
            ),
            expected_udp_traffic("h3", 8_002, Protocol::Http3, ListenerDescriptorKind::Quic),
            expected_udp_traffic(
                "forward-h3",
                8_003,
                Protocol::ForwardHttp3,
                ListenerDescriptorKind::Quic,
            ),
            ExpectedInventoryEntry {
                id: ListenerId::Management,
                name: "@management",
                protocol: Protocol::Http,
                bind: ListenerBind::Socket {
                    address: "127.0.0.1:9900".parse().expect("management bind"),
                },
                descriptor_role: ListenerDescriptorRole::Management,
                descriptor_kind: ListenerDescriptorKind::Tcp,
                descriptor_protocol: None,
                policy: (
                    ListenerPlane::Control,
                    ListenerMetricPolicy::InternalOnly,
                    None,
                ),
            },
            expected_stats(0, 8_404),
            expected_stats(1, 8_405),
            ExpectedInventoryEntry {
                id: ListenerId::StatsPage(0),
                name: "@stats-page-0",
                protocol: Protocol::Http,
                bind: ListenerBind::Socket {
                    address: "127.0.0.1:8406".parse().expect("stats page bind"),
                },
                descriptor_role: ListenerDescriptorRole::StatsPage(0),
                descriptor_kind: ListenerDescriptorKind::Tcp,
                descriptor_protocol: None,
                policy: (
                    ListenerPlane::Control,
                    ListenerMetricPolicy::Public,
                    Some(17),
                ),
            },
        ];
        let expected_slots = vec![
            expected_tcp_slot(0, DescriptorRole::Traffic("http".into()), 7_996),
            expected_tcp_slot(1, DescriptorRole::Traffic("rtmp".into()), 7_997),
            expected_tcp_slot(2, DescriptorRole::Traffic("forward-h1".into()), 7_998),
            expected_tcp_slot(3, DescriptorRole::Traffic("forward-h2".into()), 7_999),
            expected_tcp_slot(4, DescriptorRole::Traffic("tcp".into()), 8_000),
            DescriptorSlot {
                id: SlotId(5),
                role: DescriptorRole::Traffic("unix".into()),
                kind: DescriptorKind::UnixListener,
                bind: Some(BindIdentity::UnixPath(PathBuf::from("/tmp/inventory.sock"))),
                mode: Some(0o600),
            },
            expected_datagram_slot(6, "udp", 8_001, DescriptorKind::DatagramListener),
            expected_datagram_slot(7, "h3", 8_002, DescriptorKind::QuicListener),
            expected_datagram_slot(8, "forward-h3", 8_003, DescriptorKind::QuicListener),
            expected_tcp_slot(9, DescriptorRole::Management, 9_900),
            expected_tcp_slot(10, DescriptorRole::Stats(0), 8_404),
            expected_tcp_slot(11, DescriptorRole::Stats(1), 8_405),
            expected_tcp_slot(12, DescriptorRole::StatsPage(0), 8_406),
        ];
        assert_inventory_and_slots(&config, &expected_inventory, &expected_slots);

        let mut changed = config.to_draft();
        changed.listeners.swap(0, 8);
        changed.listeners[0].name = "renamed-forward-h3".into();
        let changed = changed
            .validate()
            .expect("valid reordered and renamed config");
        let renamed_reordered_inventory = vec![
            expected_udp_traffic(
                "renamed-forward-h3",
                8_003,
                Protocol::ForwardHttp3,
                ListenerDescriptorKind::Quic,
            ),
            expected_traffic(
                "rtmp",
                7_997,
                Protocol::Rtmp,
                ListenerDescriptorKind::Tcp,
                Some(8),
            ),
            expected_traffic(
                "forward-h1",
                7_998,
                Protocol::ForwardHttp1,
                ListenerDescriptorKind::Tcp,
                Some(9),
            ),
            expected_traffic(
                "forward-h2",
                7_999,
                Protocol::ForwardHttp2,
                ListenerDescriptorKind::Tcp,
                Some(10),
            ),
            expected_traffic(
                "tcp",
                8_000,
                Protocol::Tcp,
                ListenerDescriptorKind::Tcp,
                Some(11),
            ),
            ExpectedInventoryEntry {
                id: ListenerId::Traffic("unix".into()),
                name: "unix",
                protocol: Protocol::Tcp,
                bind: ListenerBind::Unix {
                    path: PathBuf::from("/tmp/inventory.sock"),
                    mode: Some(0o600),
                },
                descriptor_role: ListenerDescriptorRole::Traffic("unix".into()),
                descriptor_kind: ListenerDescriptorKind::Unix,
                descriptor_protocol: Some(Protocol::Tcp),
                policy: (ListenerPlane::Data, ListenerMetricPolicy::Public, None),
            },
            expected_udp_traffic(
                "udp",
                8_001,
                Protocol::Udp,
                ListenerDescriptorKind::Datagram,
            ),
            expected_udp_traffic("h3", 8_002, Protocol::Http3, ListenerDescriptorKind::Quic),
            expected_traffic(
                "http",
                7_996,
                Protocol::Http,
                ListenerDescriptorKind::Tcp,
                Some(7),
            ),
            ExpectedInventoryEntry {
                id: ListenerId::Management,
                name: "@management",
                protocol: Protocol::Http,
                bind: ListenerBind::Socket {
                    address: "127.0.0.1:9900".parse().expect("management bind"),
                },
                descriptor_role: ListenerDescriptorRole::Management,
                descriptor_kind: ListenerDescriptorKind::Tcp,
                descriptor_protocol: None,
                policy: (
                    ListenerPlane::Control,
                    ListenerMetricPolicy::InternalOnly,
                    None,
                ),
            },
            expected_stats(0, 8_404),
            expected_stats(1, 8_405),
            ExpectedInventoryEntry {
                id: ListenerId::StatsPage(0),
                name: "@stats-page-0",
                protocol: Protocol::Http,
                bind: ListenerBind::Socket {
                    address: "127.0.0.1:8406".parse().expect("stats page bind"),
                },
                descriptor_role: ListenerDescriptorRole::StatsPage(0),
                descriptor_kind: ListenerDescriptorKind::Tcp,
                descriptor_protocol: None,
                policy: (
                    ListenerPlane::Control,
                    ListenerMetricPolicy::Public,
                    Some(17),
                ),
            },
        ];
        let renamed_reordered_slots = vec![
            expected_datagram_slot(0, "renamed-forward-h3", 8_003, DescriptorKind::QuicListener),
            expected_tcp_slot(1, DescriptorRole::Traffic("rtmp".into()), 7_997),
            expected_tcp_slot(2, DescriptorRole::Traffic("forward-h1".into()), 7_998),
            expected_tcp_slot(3, DescriptorRole::Traffic("forward-h2".into()), 7_999),
            expected_tcp_slot(4, DescriptorRole::Traffic("tcp".into()), 8_000),
            DescriptorSlot {
                id: SlotId(5),
                role: DescriptorRole::Traffic("unix".into()),
                kind: DescriptorKind::UnixListener,
                bind: Some(BindIdentity::UnixPath(PathBuf::from("/tmp/inventory.sock"))),
                mode: Some(0o600),
            },
            expected_datagram_slot(6, "udp", 8_001, DescriptorKind::DatagramListener),
            expected_datagram_slot(7, "h3", 8_002, DescriptorKind::QuicListener),
            expected_tcp_slot(8, DescriptorRole::Traffic("http".into()), 7_996),
            expected_tcp_slot(9, DescriptorRole::Management, 9_900),
            expected_tcp_slot(10, DescriptorRole::Stats(0), 8_404),
            expected_tcp_slot(11, DescriptorRole::Stats(1), 8_405),
            expected_tcp_slot(12, DescriptorRole::StatsPage(0), 8_406),
        ];
        assert_inventory_and_slots(
            &changed,
            &renamed_reordered_inventory,
            &renamed_reordered_slots,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inventory_has_no_supervised_descriptor_limit() {
        let sixty_four = many_listener_config(MAX_DESCRIPTOR_COUNT);
        assert_eq!(ListenerInventory::compile(&sixty_four).entries().len(), 64);
        assert_eq!(
            descriptor_slots(&ListenerInventory::compile(&sixty_four))
                .expect("64 descriptors are accepted")
                .len(),
            64
        );

        let sixty_five = many_listener_config(MAX_DESCRIPTOR_COUNT + 1);
        assert_eq!(ListenerInventory::compile(&sixty_five).entries().len(), 65);
        let error = descriptor_slots(&ListenerInventory::compile(&sixty_five))
            .expect_err("existing supervised oracle rejects 65 descriptors");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("maximum is 64"));
    }

    #[test]
    fn validation_tcp_probe_holds_then_releases_the_ephemeral_address() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral TCP probe");
        let address = probe.local_addr().expect("ephemeral TCP address");
        drop(probe);
        let mut config = many_listener_config(1).to_draft();
        config.listeners[0].bind = ListenerBind::Socket { address };
        let config = config.validate().expect("valid TCP probe config");

        let reservations = ListenerReservations::prepare_for_validation(&config, None)
            .expect("validation TCP reservation");
        assert!(
            std::net::TcpListener::bind(address).is_err(),
            "validation did not hold the TCP probe"
        );
        drop(reservations);
        std::net::TcpListener::bind(address).expect("validation released the TCP probe");
    }

    #[cfg(unix)]
    #[test]
    fn validation_unix_probe_removes_its_temporary_namespace_entries() {
        let directory = tempfile::tempdir().expect("Unix validation directory");
        let path = directory.path().join("validation.sock");
        let mut config = many_listener_config(1).to_draft();
        config.listeners[0].bind = ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o600),
        };
        let config = config.validate().expect("valid Unix probe config");

        let reservations = ListenerReservations::prepare_for_validation(&config, None)
            .expect("validation Unix reservation");
        assert!(path.exists(), "validation did not create the Unix probe");
        drop(reservations);
        assert!(!path.exists(), "validation retained the Unix probe");
        assert!(
            !unix_socket_marker_path(&path)
                .expect("Unix marker path")
                .exists(),
            "validation retained the Unix lease marker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validation_source_preserves_direct_capacity_and_unix_path_only_reuse_semantics() {
        let direct = many_listener_config(MAX_DESCRIPTOR_COUNT + 1);
        assert_eq!(
            ListenerReservations::prepare_for_validation(&direct, None)
                .expect("direct validation accepts 65 listeners")
                .len(),
            65
        );

        let directory = tempfile::tempdir().expect("validation source directory");
        let path = directory.path().join("validation-source.sock");
        let mut active_config = empty_config();
        active_config.listeners.push(oxiroute_config::Listener {
            name: "unix".into(),
            bind: ListenerBind::Unix {
                path: path.clone(),
                mode: Some(0o600),
            },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let active = prepare_reservations(&active_config, None).expect("active Unix reservation");
        let mut changed = active_config.clone();
        changed.listeners[0].bind = ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o660),
        };
        let changed = validated_test_config(&changed).expect("changed Unix validation config");

        let validation = ListenerReservations::prepare_for_validation(&changed, Some(&active))
            .expect("validation reuses active Unix path");
        assert!(Arc::ptr_eq(
            &active.get("unix").expect("active Unix reservation").inner,
            &validation
                .get("unix")
                .expect("validation Unix reservation")
                .inner,
        ));
        assert!(ListenerReservations::prepare(&changed, Some(&active)).is_err());
        drop(validation);
        assert!(path.exists(), "validation unlinked the active Unix socket");
        drop(active);
        assert!(!path.exists(), "active owner retained the Unix socket");
    }

    fn prepare_reservations(
        config: &ConfigDraft,
        previous: Option<&ListenerReservations>,
    ) -> io::Result<ListenerReservations> {
        let config = validated_test_config(config)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        ListenerReservations::prepare(&config, previous)
    }

    #[cfg(target_os = "linux")]
    fn adopt_reservations(
        config: &ConfigDraft,
        descriptors: DescriptorSet,
    ) -> io::Result<ListenerReservations> {
        let config = validated_test_config(config)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        ListenerReservations::adopt(&config, descriptors)
    }

    #[cfg(target_os = "linux")]
    fn export_reservations(
        reservations: &ListenerReservations,
        config: &ConfigDraft,
    ) -> io::Result<(DescriptorManifest, Vec<std::os::fd::OwnedFd>)> {
        let config = validated_test_config(config)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        reservations.export_descriptors(&ListenerInventory::compile(&config))
    }

    fn validated_test_config(
        config: &ConfigDraft,
    ) -> Result<ValidatedConfig, oxiroute_config::ConfigError> {
        let mut complete = differential_config().to_draft();
        complete.listeners = config.listeners.clone();
        complete.management.clone_from(&config.management);
        complete.stats.clone_from(&config.stats);
        for listener in &mut complete.listeners {
            match listener.protocol {
                Protocol::Http | Protocol::Http3 => listener.service = Some("web".into()),
                Protocol::Rtmp => listener.service = Some("live".into()),
                Protocol::Tcp | Protocol::Udp => listener.service = Some("relay".into()),
                Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3 => {
                    listener.service = Some("forward".into());
                }
            }
            if matches!(listener.protocol, Protocol::Http3 | Protocol::ForwardHttp3) {
                listener.tls_profile = Some("h3".into());
            }
        }
        complete.validate()
    }

    #[test]
    fn matching_listener_reservations_are_reused_without_rebinding() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first =
            prepare_reservations(&config("edge", address), None).expect("first reservation");
        let second = prepare_reservations(&config("edge", address), Some(&first))
            .expect("reused reservation");

        assert!(Arc::ptr_eq(
            &first.get("edge").expect("first").inner,
            &second.get("edge").expect("second").inner
        ));
    }

    #[cfg(unix)]
    #[test]
    fn udp_listener_reservation_owns_and_releases_the_datagram_socket() {
        let address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary UDP bind")
            .local_addr()
            .expect("UDP address");
        let bind = ListenerBind::Udp { address };
        let reservation =
            ListenerReservation::bind("datagram", &bind).expect("UDP listener reservation");
        let duplicate = reservation
            .duplicate_udp_socket()
            .expect("UDP descriptor duplicate");
        assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
        assert_eq!(
            std::net::UdpSocket::bind(address)
                .expect_err("reserved UDP address must remain owned")
                .kind(),
            io::ErrorKind::AddrInUse
        );
        drop(duplicate);
        drop(reservation);
        std::net::UdpSocket::bind(address).expect("UDP address released");
    }

    #[test]
    fn listener_reservations_are_reused_after_rename() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let first =
            prepare_reservations(&config("old-name", address), None).expect("first reservation");
        let second = prepare_reservations(&config("new-name", address), Some(&first))
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
        let first = prepare_reservations(&first_config, None).expect("first reservations");
        let mut second_config = first_config;
        second_config.stats.as_mut().expect("stats").binds.reverse();
        let second =
            prepare_reservations(&second_config, Some(&first)).expect("reordered reservations");

        assert!(Arc::ptr_eq(
            &first.stats(0).expect("first original").inner,
            &second.stats(1).expect("first reordered").inner,
        ));
        assert!(Arc::ptr_eq(
            &first.get("@stats-0").expect("legacy first original").inner,
            &first.stats(0).expect("typed first original").inner,
        ));
        assert!(Arc::ptr_eq(
            &first.stats(1).expect("second original").inner,
            &second.stats(0).expect("second reordered").inner,
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

        let first = prepare_reservations(&config, None).expect("first reservations");
        let second = prepare_reservations(&config, Some(&first)).expect("reused page reservation");

        assert_eq!(first.len(), 1);
        assert!(Arc::ptr_eq(
            &first.stats_page(0).expect("first page").inner,
            &second.stats_page(0).expect("second page").inner,
        ));
        assert!(Arc::ptr_eq(
            &first.get("@stats-page-0").expect("legacy first page").inner,
            &first.stats_page(0).expect("typed first page").inner,
        ));
    }

    #[test]
    fn traffic_name_cannot_collide_with_management_reservation() {
        let traffic = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("traffic temporary bind")
            .local_addr()
            .expect("traffic address");
        let management = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("management temporary bind")
            .local_addr()
            .expect("management address");
        let mut config = config("@management", traffic);
        config.management = Some(oxiroute_config::Management {
            bind: management,
            ui_dir: None,
        });

        let reservations = prepare_reservations(&config, None).expect("reservations");

        assert_eq!(reservations.len(), 2);
        assert_eq!(
            reservations
                .get("@management")
                .expect("traffic reservation")
                .bind_config(),
            &ListenerBind::Socket { address: traffic }
        );
        assert_eq!(
            reservations
                .management()
                .expect("management reservation")
                .bind_config(),
            &ListenerBind::Socket {
                address: management
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_export_is_canonical_typed_and_cloexec() {
        use rustix::io::{FdFlags, fcntl_getfd};

        let traffic = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("traffic temporary bind")
            .local_addr()
            .expect("traffic address");
        let management = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("management temporary bind")
            .local_addr()
            .expect("management address");
        let stats = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("stats temporary bind")
            .local_addr()
            .expect("stats address");
        let page = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("page temporary bind")
            .local_addr()
            .expect("page address");
        let mut config = config("@management", traffic);
        config.management = Some(oxiroute_config::Management {
            bind: management,
            ui_dir: None,
        });
        config.stats = Some(oxiroute_config::Stats {
            binds: vec![stats],
            admin_token_file: None,
            pages: vec![oxiroute_config::StatsPage {
                bind: page,
                uri_prefix: "/stats".into(),
                refresh_ms: 1_000,
                admin: oxiroute_config::StatsPageAdminPolicy::Disabled,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            }],
        });
        let reservations = prepare_reservations(&config, None).expect("reservations");

        let (manifest, originals) =
            export_reservations(&reservations, &config).expect("descriptor export");

        assert_eq!(originals.len(), 4);
        assert_eq!(
            manifest
                .slots()
                .iter()
                .map(|slot| (slot.id, slot.role.clone()))
                .collect::<Vec<_>>(),
            vec![
                (SlotId(0), DescriptorRole::Traffic("@management".into())),
                (SlotId(1), DescriptorRole::Management),
                (SlotId(2), DescriptorRole::Stats(0)),
                (SlotId(3), DescriptorRole::StatsPage(0)),
            ]
        );
        assert!(originals.iter().all(|descriptor| {
            fcntl_getfd(descriptor)
                .expect("descriptor flags")
                .contains(FdFlags::CLOEXEC)
        }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_export_and_adoption_supports_udp_relay_listeners() {
        let socket =
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("temporary UDP bind");
        socket
            .set_nonblocking(true)
            .expect("nonblocking UDP socket");
        let address = socket.local_addr().expect("UDP address");
        drop(socket);
        let mut config = config("udp", address);
        config.listeners[0].bind = ListenerBind::Udp { address };
        config.listeners[0].protocol = oxiroute_config::Protocol::Udp;
        let reservations = prepare_reservations(&config, None).expect("reservations");
        let (manifest, originals) =
            export_reservations(&reservations, &config).expect("UDP descriptor export");

        assert_eq!(manifest.slots()[0].kind, DescriptorKind::DatagramListener);
        assert_eq!(manifest.slots()[0].bind, Some(BindIdentity::Tcp(address)));
        let descriptors = DescriptorSet::new(&manifest, originals).expect("UDP descriptor set");
        let adopted = adopt_reservations(&config, descriptors).expect("UDP adoption");
        let duplicate = adopted
            .get("udp")
            .expect("adopted UDP reservation")
            .duplicate_udp_socket()
            .expect("adopted UDP duplicate");
        assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_export_and_adoption_supports_quic_listeners() {
        let socket =
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("temporary QUIC bind");
        socket
            .set_nonblocking(true)
            .expect("nonblocking QUIC socket");
        let address = socket.local_addr().expect("QUIC address");
        drop(socket);
        let mut config = config("h3", address);
        config.listeners[0].bind = ListenerBind::Udp { address };
        config.listeners[0].protocol = oxiroute_config::Protocol::Http3;
        let reservations = prepare_reservations(&config, None).expect("reservations");
        let (manifest, originals) =
            export_reservations(&reservations, &config).expect("QUIC descriptor export");

        assert_eq!(manifest.slots()[0].kind, DescriptorKind::QuicListener);
        assert_eq!(manifest.slots()[0].bind, Some(BindIdentity::Tcp(address)));
        let descriptors = DescriptorSet::new(&manifest, originals).expect("QUIC descriptor set");
        let adopted = adopt_reservations(&config, descriptors).expect("QUIC adoption");
        let duplicate = adopted
            .get("h3")
            .expect("adopted QUIC reservation")
            .duplicate_udp_socket()
            .expect("adopted QUIC duplicate");
        assert_eq!(duplicate.local_addr().expect("duplicate address"), address);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_adoption_rejects_a_wrong_typed_slot() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let config = config("traffic", address);
        let reservations = prepare_reservations(&config, None).expect("reservations");
        let (manifest, originals) =
            export_reservations(&reservations, &config).expect("descriptor export");
        let mut wrong_slots = manifest.slots().to_vec();
        wrong_slots[0].role = DescriptorRole::Management;
        let wrong = DescriptorManifest::new(wrong_slots).expect("wrong typed manifest");
        let set = DescriptorSet::new(&wrong, originals).expect("kernel-valid descriptor set");

        let error = adopt_reservations(&config, set)
            .err()
            .expect("typed slot mismatch");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_export_rejects_extra_keys_and_bind_mismatch() {
        let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("temporary bind")
            .local_addr()
            .expect("address");
        let config = config("traffic", address);
        let mut reservations = prepare_reservations(&config, None).expect("reservations");
        let traffic = reservations.get("traffic").expect("traffic").clone();
        reservations
            .by_key
            .insert(ReservationKey::Management, traffic);
        let error = export_reservations(&reservations, &config).expect_err("extra key must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        drop(reservations);

        let reservations = prepare_reservations(&config, None).expect("reservations");
        let mut changed = config;
        changed.listeners[0].bind = ListenerBind::Socket {
            address: "127.0.0.1:1".parse().expect("changed address"),
        };
        let error =
            export_reservations(&reservations, &changed).expect_err("bind mismatch must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn worker_descriptor_limit_is_explicitly_sixty_four() {
        let mut config = empty_config();
        config.listeners = (0..=MAX_DESCRIPTOR_COUNT)
            .map(|index| oxiroute_config::Listener {
                name: format!("listener-{index}"),
                bind: ListenerBind::Socket {
                    address: SocketAddr::from((
                        [127, 0, 0, 1],
                        10_000 + u16::try_from(index).expect("bounded listener index"),
                    )),
                },
                protocol: oxiroute_config::Protocol::Http,
                service: Some("http".into()),
                tls_profile: None,
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            })
            .collect();

        let config = validated_test_config(&config).expect("valid oversized config");
        let error = descriptor_slots(&ListenerInventory::compile(&config))
            .expect_err("65 descriptors must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("maximum is 64"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descriptor_capacity_calculation_includes_amplification_and_headroom() {
        let mut config = config("tcp", "127.0.0.1:8080".parse().expect("address"));
        config.listeners.push(oxiroute_config::Listener {
            name: "unix".into(),
            bind: ListenerBind::Unix {
                path: PathBuf::from("/tmp/capacity.sock"),
                mode: Some(0o600),
            },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let config = validated_test_config(&config).expect("valid capacity config");
        let inventory = ListenerInventory::compile(&config);

        assert_eq!(descriptor_capacity_required(&inventory, 11), 50);
        assert!(validate_descriptor_capacity(&inventory, 11, Some(50)).is_ok());
        assert!(validate_descriptor_capacity(&inventory, 11, None).is_ok());
        let error = validate_descriptor_capacity(&inventory, 11, Some(49))
            .expect_err("low descriptor limit must fail");
        assert!(error.to_string().contains("at least 50"));
        assert!(error.to_string().contains("11 currently open"));
        assert!(error.to_string().contains("soft limit is 49"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adopted_unix_reservation_never_owns_master_namespace_cleanup() {
        let directory = tempfile::tempdir().expect("Unix reservation directory");
        let path = directory.path().join("adopted.sock");
        let mut config = empty_config();
        config.listeners.push(oxiroute_config::Listener {
            name: "unix".into(),
            bind: ListenerBind::Unix {
                path: path.clone(),
                mode: Some(0o600),
            },
            protocol: oxiroute_config::Protocol::Http,
            service: Some("http".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let master = prepare_reservations(&config, None).expect("master reservation");
        let (manifest, originals) =
            export_reservations(&master, &config).expect("descriptor export");
        let set = DescriptorSet::new(&manifest, originals).expect("descriptor set");
        let worker = adopt_reservations(&config, set).expect("worker adoption");
        let marker = unix_socket_marker_path(&path).expect("marker path");

        drop(worker);
        assert!(path.exists(), "worker drop unlinked the master socket");
        assert!(marker.exists(), "worker drop unlinked the master lease");

        drop(master);
        assert!(!path.exists(), "master drop retained the socket");
        assert!(!marker.exists(), "master drop retained the lease marker");
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
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        });
        let first = prepare_reservations(&first_config, None).expect("first Unix bind");
        let unchanged =
            prepare_reservations(&first_config, Some(&first)).expect("identical Unix bind reuse");
        assert!(Arc::ptr_eq(
            &first.get("unix").expect("first").inner,
            &unchanged.get("unix").expect("unchanged").inner,
        ));

        let mut changed_config = first_config.clone();
        changed_config.listeners[0].bind = ListenerBind::Unix {
            path: path.clone(),
            mode: Some(0o660),
        };
        let validated_config =
            validated_test_config(&changed_config).expect("valid changed Unix config");
        let validated =
            ListenerReservations::prepare_for_validation(&validated_config, Some(&first))
                .expect("validation reuses the active Unix path");
        assert!(Arc::ptr_eq(
            &first.get("unix").expect("first").inner,
            &validated.get("unix").expect("validated").inner,
        ));
        assert!(ListenerReservations::listener_restart_required(
            crate::RuntimeMode::Direct,
            &validated_test_config(&first_config).expect("valid first config"),
            &validated_config,
        ));
        changed_config.listeners[0].max_connections = Some(7);
        assert!(ListenerReservations::listener_restart_required(
            crate::RuntimeMode::Direct,
            &validated_test_config(&first_config).expect("valid first config"),
            &validated_test_config(&changed_config).expect("valid changed config"),
        ));
        assert!(prepare_reservations(&changed_config, Some(&first)).is_err());
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
        let lease = UnixSocketLease::acquire(&path).expect("Unix namespace lease");
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
