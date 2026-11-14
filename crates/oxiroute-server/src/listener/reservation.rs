use std::{collections::HashMap, io, sync::Arc};

use log::warn;
use oxiroute_config::{ListenerBind, ValidatedConfig};
#[cfg(target_os = "linux")]
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSet,
    DescriptorSlot, MAX_DESCRIPTOR_COUNT, SlotId,
};
#[cfg(target_os = "linux")]
use oxiroute_supervisor_master::StableListeners;

use crate::listener::inventory::{
    ListenerChangeSummary, ListenerDescriptorKind, ListenerDescriptorRole, ListenerId,
    ListenerInventory,
};

#[cfg(unix)]
mod quic;
#[cfg(unix)]
mod unix_socket;
#[cfg(all(unix, test))]
use unix_socket::unix_socket_marker_path;
#[cfg(unix)]
use unix_socket::{PathIdentity, UnixSocketIdentity, UnixSocketLease, remove_stale_unix_socket};

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
    #[cfg(unix)]
    quic: std::sync::Mutex<Option<quic::QuicHost>>,
}

#[cfg(unix)]
enum ReservedSocket {
    Tcp(std::net::TcpListener),
    Udp(std::net::UdpSocket),
    Unix(std::os::unix::net::UnixListener),
}

/// Whether the UDP sockets of a configuration join a `SO_REUSEPORT` group: its listeners have several workers, so an
/// HTTP/3 listener runs several QUIC endpoints.
fn shares_udp(config: &ValidatedConfig) -> bool {
    config
        .as_draft()
        .listener_workers
        .is_some_and(|workers| workers > 1)
}

/// A non-blocking listening TCP socket on `address`. `[::]` is dual-stack whatever the host's `bindv6only` says.
#[cfg(unix)]
fn bind_tcp(address: std::net::SocketAddr) -> io::Result<std::net::TcpListener> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, listen, socket_with, sockopt};

    let family = if address.is_ipv4() {
        AddressFamily::INET
    } else {
        AddressFamily::INET6
    };
    let socket = socket_with(
        family,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    sockopt::set_socket_reuseaddr(&socket, true)?;
    if address.ip() == std::net::Ipv6Addr::UNSPECIFIED {
        sockopt::set_ipv6_v6only(&socket, false)?;
    }
    bind(&socket, &address)?;
    listen(&socket, -1)?;
    Ok(socket.into())
}

/// A non-blocking UDP socket bound to `address`; with `share` it joins the `SO_REUSEPORT` group of that address, whose
/// sockets the kernel gives the peers of its datagrams to, one socket per peer address.
#[cfg(unix)]
fn bind_udp(address: std::net::SocketAddr, share: bool) -> io::Result<std::net::UdpSocket> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, socket_with, sockopt};

    let family = if address.is_ipv4() {
        AddressFamily::INET
    } else {
        AddressFamily::INET6
    };
    let socket = socket_with(
        family,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    if share {
        sockopt::set_socket_reuseport(&socket, true)?;
    }
    bind(&socket, &address)?;
    Ok(socket.into())
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
        Self::bind_with(listener_name, bind, false)
    }

    /// [`ListenerReservation::bind`]; with `share_udp` a UDP socket joins a `SO_REUSEPORT` group, so that
    /// [`ListenerReservation::quic_endpoints`] can bind siblings to it. Other transports ignore it.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be bound securely or the transport is unsupported.
    #[allow(clippy::too_many_lines)]
    pub fn bind_with(
        listener_name: &str,
        bind: &ListenerBind,
        share_udp: bool,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let (socket, unix_socket, bind_text) = match bind {
                ListenerBind::Socket { address } => {
                    let listener = bind_tcp(*address).map_err(|source| {
                        io::Error::new(
                            source.kind(),
                            format!(
                                "listener `{listener_name}` could not bind socket `{address}`: {source}"
                            ),
                        )
                    })?;
                    (ReservedSocket::Tcp(listener), None, address.to_string())
                }
                ListenerBind::Udp { address } => {
                    let socket = bind_udp(*address, share_udp).map_err(|source| {
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
                    quic: std::sync::Mutex::default(),
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

    /// Returns the QUIC endpoints that read this UDP reservation, starting them on first use: `count` of them when
    /// the socket joined a `SO_REUSEPORT` group at bind (see [`ListenerReservation::bind_with`]), else one.
    ///
    /// Every generation that reuses the reservation shares the endpoints, so a socket never has
    /// two readers. `config` is only used to start the endpoints; connections are accepted with
    /// their generation's own configuration. The endpoints of a host are the ones it started with:
    /// a later call with another `count` gets those.
    ///
    /// # Errors
    ///
    /// Returns an error when this reservation is not a UDP socket or an endpoint cannot start.
    #[cfg(unix)]
    pub fn quic_endpoints(
        &self,
        config: &quinn::ServerConfig,
        count: usize,
    ) -> io::Result<Vec<quinn::Endpoint>> {
        let mut host = self
            .inner
            .quic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let started = match host.take() {
            Some(started) => started,
            None => quic::QuicHost::start(self.udp_group(count)?, config)?,
        };
        let endpoints = started.endpoints();
        *host = Some(started);
        Ok(endpoints)
    }

    #[cfg(not(unix))]
    pub fn quic_endpoints(
        &self,
        _config: &quinn::ServerConfig,
        _count: usize,
    ) -> io::Result<Vec<quinn::Endpoint>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "UDP listener reservations are unsupported on this platform",
        ))
    }

    /// The reservation's socket and, when it is in a `SO_REUSEPORT` group, `count - 1` siblings bound to the same
    /// address.
    #[cfg(unix)]
    fn udp_group(&self, count: usize) -> io::Result<Vec<std::net::UdpSocket>> {
        let first = self.duplicate_udp_socket()?;
        if count <= 1 {
            return Ok(vec![first]);
        }
        if !rustix::net::sockopt::socket_reuseport(&first)? {
            warn!(
                "listener `{}` has {count} workers but its UDP socket was not bound for sharing: one QUIC endpoint serves it",
                self.inner.bind_text
            );
            return Ok(vec![first]);
        }
        let address = first.local_addr()?;
        let mut group = vec![first];
        for _ in 1..count {
            group.push(bind_udp(address, true)?);
        }
        Ok(group)
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
                quic: std::sync::Mutex::default(),
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
        Self::prepare_inventory(
            &ListenerInventory::compile(config),
            previous,
            (false, shares_udp(config)),
        )
    }

    pub(crate) fn prepare_for_validation(
        config: &ValidatedConfig,
        previous: Option<&Self>,
    ) -> io::Result<Self> {
        // Validation intentionally performs real, temporary bind probes. Dropping the returned
        // set closes descriptors and removes any Unix socket namespace entries it created.
        Self::prepare_inventory(
            &ListenerInventory::compile(config),
            previous,
            (true, shares_udp(config)),
        )
    }

    fn prepare_inventory(
        inventory: &ListenerInventory,
        previous: Option<&Self>,
        (reuse_unix_path, share_udp): (bool, bool),
    ) -> io::Result<Self> {
        let mut by_key = HashMap::with_capacity(inventory.len());
        for entry in inventory.entries() {
            let reservation = previous
                .and_then(|reservations| reservations.by_bind(&entry.bind, reuse_unix_path))
                .cloned()
                .map_or_else(
                    || ListenerReservation::bind_with(&entry.name, &entry.bind, share_udp),
                    Ok,
                )?;
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
        _mode: crate::RuntimeMode,
        active: &ValidatedConfig,
        candidate: &ValidatedConfig,
    ) -> bool {
        // Both runtimes apply listener changes on reload; the mode is kept for API stability.
        ListenerInventory::compile(active).restart_required(&ListenerInventory::compile(candidate))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn same_supervised_listener_topology(
        active: &ValidatedConfig,
        candidate: &ValidatedConfig,
    ) -> bool {
        ListenerInventory::compile(active)
            .same_descriptor_topology(&ListenerInventory::compile(candidate))
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

/// Describes what a reload from `active` to `candidate` does to the listener sockets.
///
/// Returns `None` when either inventory cannot be described as a descriptor manifest (for example
/// above the descriptor limit); validation reports that separately.
#[cfg(target_os = "linux")]
pub(crate) fn listener_changes(
    active: &ListenerInventory,
    candidate: &ListenerInventory,
) -> Option<ListenerChangeSummary> {
    let current = DescriptorManifest::new(descriptor_slots(active).ok()?).ok()?;
    let next = DescriptorManifest::new(descriptor_slots(candidate).ok()?).ok()?;
    let changes = current.changes(&next);
    Some(ListenerChangeSummary {
        added: changes.added,
        removed: changes.removed,
        kept: changes.kept,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn listener_changes(
    _active: &ListenerInventory,
    _candidate: &ListenerInventory,
) -> Option<ListenerChangeSummary> {
    None
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
mod tests;
