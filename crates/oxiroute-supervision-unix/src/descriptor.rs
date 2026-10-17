use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    os::{
        fd::{AsFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::PathBuf,
};

use rustix::{
    fs::{FileType, OFlags, fcntl_getfl, fstat},
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
    net::{AddressFamily, SocketAddrUnix, SocketType, getsockname, sockopt},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::MAX_DESCRIPTOR_COUNT;

/// Stable identifier for one descriptor slot in a manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SlotId(pub u16);

/// Descriptor capabilities required by a typed manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DescriptorCapabilities(u16);

impl DescriptorCapabilities {
    /// Stream listener descriptors.
    pub const STREAM: Self = Self(0x0001);
    /// Generic datagram listener descriptors.
    pub const DATAGRAM: Self = Self(0x0002);
    /// QUIC listener descriptors.
    pub const QUIC: Self = Self(0x0004);
    /// Stream and generic datagram listener descriptors.
    pub const STREAM_AND_DATAGRAM: Self = Self(Self::STREAM.0 | Self::DATAGRAM.0);
    /// Every descriptor capability defined by this manifest protocol.
    pub const ALL: Self = Self(Self::STREAM.0 | Self::DATAGRAM.0 | Self::QUIC.0);

    /// Returns the wire representation.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns whether all capabilities in `required` are present.
    #[must_use]
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Reconstructs capabilities from a bounded wire value.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Semantic purpose of a transferred descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "role", content = "name")]
pub enum DescriptorRole {
    /// A configured traffic listener.
    Traffic(String),
    /// The configuration-management listener.
    Management,
    /// One HAProxy-compatible statistics API listener.
    Stats(u16),
    /// One public statistics page listener.
    StatsPage(u16),
    /// A state or snapshot file.
    State,
    /// A supervision control channel.
    Control,
}

/// Kernel object shape required for a descriptor slot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorKind {
    /// Listening IPv4 or IPv6 stream socket under trusted exclusive ownership.
    TcpListener,
    /// Listening filesystem or abstract Unix stream socket under trusted exclusive ownership.
    UnixListener,
    /// Listening IPv4 or IPv6 datagram socket for a UDP relay.
    DatagramListener,
    /// Listening IPv4 or IPv6 datagram socket reserved for QUIC/H3.
    QuicListener,
    /// No listener-specific validation beyond manifest ownership.
    Opaque,
}

impl DescriptorKind {
    const fn capability(self) -> DescriptorCapabilities {
        match self {
            Self::TcpListener | Self::UnixListener => DescriptorCapabilities::STREAM,
            Self::DatagramListener => DescriptorCapabilities::DATAGRAM,
            Self::QuicListener => DescriptorCapabilities::QUIC,
            Self::Opaque => DescriptorCapabilities(0),
        }
    }
}

/// Optional exact local address expected for a listener descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "family", content = "address")]
pub enum BindIdentity {
    /// IPv4 or IPv6 socket address.
    Tcp(SocketAddr),
    /// Filesystem Unix socket path, represented losslessly on Unix.
    UnixPath(PathBuf),
    /// Linux abstract Unix socket name.
    UnixAbstract(Vec<u8>),
}

/// One ordered descriptor requirement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DescriptorSlot {
    /// Unique manifest slot identifier.
    pub id: SlotId,
    /// Semantic descriptor purpose.
    pub role: DescriptorRole,
    /// Required kernel object shape.
    pub kind: DescriptorKind,
    /// Optional exact listener bind address.
    pub bind: Option<BindIdentity>,
    /// Optional filesystem Unix socket mode.
    pub mode: Option<u16>,
}

/// Validated ordered descriptor requirements for one frame.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "Vec<DescriptorSlot>", into = "Vec<DescriptorSlot>")]
pub struct DescriptorManifest {
    slots: Vec<DescriptorSlot>,
}

impl DescriptorManifest {
    /// Validates count, unique IDs, and kind/bind compatibility.
    ///
    /// # Errors
    ///
    /// Returns an error for too many slots, duplicate IDs, or incompatible bind metadata.
    pub fn new(slots: Vec<DescriptorSlot>) -> Result<Self, DescriptorError> {
        if slots.len() > MAX_DESCRIPTOR_COUNT {
            return Err(DescriptorError::ManifestTooLarge {
                actual: slots.len(),
                maximum: MAX_DESCRIPTOR_COUNT,
            });
        }
        let mut ids = BTreeSet::new();
        for slot in &slots {
            if !ids.insert(slot.id) {
                return Err(DescriptorError::DuplicateSlot { slot: slot.id });
            }
            let compatible = matches!(
                (slot.kind, slot.bind.as_ref()),
                (_, None)
                    | (
                        DescriptorKind::TcpListener
                            | DescriptorKind::DatagramListener
                            | DescriptorKind::QuicListener,
                        Some(BindIdentity::Tcp(_)),
                    )
                    | (
                        DescriptorKind::UnixListener,
                        Some(BindIdentity::UnixPath(_) | BindIdentity::UnixAbstract(_))
                    )
            );
            if !compatible {
                return Err(DescriptorError::IncompatibleBind { slot: slot.id });
            }
            if slot.mode.is_some()
                && !matches!(
                    (slot.kind, slot.bind.as_ref()),
                    (
                        DescriptorKind::UnixListener,
                        Some(BindIdentity::UnixPath(_))
                    )
                )
            {
                return Err(DescriptorError::IncompatibleMode { slot: slot.id });
            }
        }
        Ok(Self { slots })
    }

    /// Returns ordered descriptor requirements.
    #[must_use]
    pub fn slots(&self) -> &[DescriptorSlot] {
        &self.slots
    }

    /// Returns the capabilities required by the ordered slots.
    #[must_use]
    pub fn capabilities(&self) -> DescriptorCapabilities {
        self.slots
            .iter()
            .fold(DescriptorCapabilities(0), |all, slot| {
                all.union(slot.kind.capability())
            })
    }

    /// Compares this manifest with the next one by the kernel socket each slot stands for.
    ///
    /// Two slots refer to the same socket when their kind, bind identity and Unix mode are equal,
    /// whatever their slot identifier or role (a renamed listener keeps its socket). A slot without
    /// a bind identity is matched by role instead. Slots are paired in manifest order, so a
    /// manifest that repeats one identity pairs the repeats one to one.
    #[must_use]
    pub fn diff(&self, next: &Self) -> ManifestDiff {
        let mut unmatched = vec![true; next.slots.len()];
        let mut diff = ManifestDiff::default();
        for current in &self.slots {
            let partner =
                next.slots.iter().enumerate().find(|(index, candidate)| {
                    unmatched[*index] && same_socket(current, candidate)
                });
            if let Some((index, candidate)) = partner {
                unmatched[index] = false;
                diff.kept.push((current.id, candidate.id));
            } else {
                diff.removed.push(current.id);
            }
        }
        diff.added = next
            .slots
            .iter()
            .zip(unmatched)
            .filter_map(|(slot, unmatched)| unmatched.then_some(slot.id))
            .collect();
        diff
    }
}

impl DescriptorManifest {
    /// Describes what moving from this manifest to `next` does to the listeners, for operators.
    ///
    /// Each entry is a [`DescriptorSlot::label`]. A kept listener is labelled as the next manifest
    /// names it, so a renamed listener shows its new name.
    #[must_use]
    pub fn changes(&self, next: &Self) -> ListenerChanges {
        let diff = self.diff(next);
        let label = |manifest: &Self, ids: &mut dyn Iterator<Item = SlotId>| {
            ids.filter_map(|id| manifest.slots.iter().find(|slot| slot.id == id))
                .map(DescriptorSlot::label)
                .collect::<Vec<_>>()
        };
        ListenerChanges {
            added: label(next, &mut diff.added.iter().copied()),
            removed: label(self, &mut diff.removed.iter().copied()),
            kept: label(next, &mut diff.kept.iter().map(|(_, next)| *next)),
        }
    }
}

impl DescriptorSlot {
    /// Returns a label such as `web (tcp 127.0.0.1:8080)` or `@management (unix /run/m.sock)`.
    #[must_use]
    pub fn label(&self) -> String {
        let name = match &self.role {
            DescriptorRole::Traffic(name) => name.clone(),
            DescriptorRole::Management => "@management".to_owned(),
            DescriptorRole::Stats(index) => format!("@stats-{index}"),
            DescriptorRole::StatsPage(index) => format!("@stats-page-{index}"),
            DescriptorRole::State => "@state".to_owned(),
            DescriptorRole::Control => "@control".to_owned(),
        };
        let kind = match self.kind {
            DescriptorKind::TcpListener => "tcp",
            DescriptorKind::UnixListener => "unix",
            DescriptorKind::DatagramListener => "udp",
            DescriptorKind::QuicListener => "quic",
            DescriptorKind::Opaque => "opaque",
        };
        match &self.bind {
            Some(BindIdentity::Tcp(address)) => format!("{name} ({kind} {address})"),
            Some(BindIdentity::UnixPath(path)) => format!("{name} ({kind} {})", path.display()),
            Some(BindIdentity::UnixAbstract(abstract_name)) => {
                format!(
                    "{name} ({kind} @{})",
                    String::from_utf8_lossy(abstract_name)
                )
            }
            None => name,
        }
    }
}

/// Operator-facing summary of a listener replacement, as [`DescriptorSlot::label`] strings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListenerChanges {
    /// Listeners the replacement binds.
    pub added: Vec<String>,
    /// Listeners the replacement stops accepting on and closes after draining.
    pub removed: Vec<String>,
    /// Listeners whose sockets the replacement carries over untouched.
    pub kept: Vec<String>,
}

impl ListenerChanges {
    /// Returns whether the replacement adds or removes any listener.
    #[must_use]
    pub fn changes_listeners(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }
}

fn same_socket(left: &DescriptorSlot, right: &DescriptorSlot) -> bool {
    left.kind == right.kind
        && left.bind == right.bind
        && left.mode == right.mode
        && (left.bind.is_some() || left.role == right.role)
}

/// Difference between two listener manifests, expressed in slot identifiers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ManifestDiff {
    /// `(current, next)` slot pairs that stand for the same socket.
    pub kept: Vec<(SlotId, SlotId)>,
    /// Slots of the next manifest whose socket the current manifest does not own.
    pub added: Vec<SlotId>,
    /// Slots of the current manifest whose socket the next manifest no longer lists.
    pub removed: Vec<SlotId>,
}

impl ManifestDiff {
    /// Returns whether both manifests list exactly the same sockets.
    #[must_use]
    pub fn is_unchanged(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl TryFrom<Vec<DescriptorSlot>> for DescriptorManifest {
    type Error = DescriptorError;

    fn try_from(slots: Vec<DescriptorSlot>) -> Result<Self, Self::Error> {
        Self::new(slots)
    }
}

impl From<DescriptorManifest> for Vec<DescriptorSlot> {
    fn from(manifest: DescriptorManifest) -> Self {
        manifest.slots
    }
}

#[derive(Debug)]
struct DescriptorEntry {
    slot: DescriptorSlot,
    descriptor: Option<OwnedFd>,
}

/// Manifest-bound descriptor ownership; each slot can be consumed exactly once.
#[derive(Debug)]
pub struct DescriptorSet {
    entries: BTreeMap<SlotId, DescriptorEntry>,
}

impl DescriptorSet {
    /// Validates exact cardinality and every listener before exposing any descriptor.
    ///
    /// All descriptors are closed if validation fails.
    /// Listener `O_NONBLOCK` is set and verified during validation. File status flags belong to the
    /// shared open-file description, so a sender retaining a duplicate can change them later.
    /// Callers must only accept listener descriptors from a trusted sender that transfers exclusive
    /// ownership and does not retain or mutate another duplicate after handoff.
    ///
    /// # Errors
    ///
    /// Returns an error for cardinality mismatch or any failed descriptor requirement.
    pub fn new(
        manifest: &DescriptorManifest,
        descriptors: Vec<OwnedFd>,
    ) -> Result<Self, DescriptorError> {
        if descriptors.len() != manifest.slots.len() {
            return Err(DescriptorError::CardinalityMismatch {
                expected: manifest.slots.len(),
                actual: descriptors.len(),
            });
        }
        for (slot, descriptor) in manifest.slots.iter().zip(&descriptors) {
            let flags = inspect(slot.id, fcntl_getfd(descriptor))?;
            if !flags.contains(FdFlags::CLOEXEC) {
                inspect(slot.id, fcntl_setfd(descriptor, flags | FdFlags::CLOEXEC))?;
            }
            if !inspect(slot.id, fcntl_getfd(descriptor))?.contains(FdFlags::CLOEXEC) {
                return Err(DescriptorError::CloexecUnavailable { slot: slot.id });
            }
            validate_descriptor(slot, descriptor)?;
        }
        let entries = manifest
            .slots
            .iter()
            .zip(descriptors)
            .map(|(slot, descriptor)| {
                (
                    slot.id,
                    DescriptorEntry {
                        slot: slot.clone(),
                        descriptor: Some(descriptor),
                    },
                )
            })
            .collect();
        Ok(Self { entries })
    }

    /// Returns the role assigned to a known slot.
    #[must_use]
    pub fn role(&self, slot: SlotId) -> Option<&DescriptorRole> {
        self.entries.get(&slot).map(|entry| &entry.slot.role)
    }

    /// Returns the exact validated requirement assigned to a known slot.
    #[must_use]
    pub fn slot(&self, slot: SlotId) -> Option<&DescriptorSlot> {
        self.entries.get(&slot).map(|entry| &entry.slot)
    }

    /// Takes ownership from one slot exactly once.
    ///
    /// # Errors
    ///
    /// Returns an error when the slot is unknown or was already consumed.
    pub fn take(&mut self, slot: SlotId) -> Result<OwnedFd, DescriptorError> {
        let entry = self
            .entries
            .get_mut(&slot)
            .ok_or(DescriptorError::UnknownSlot { slot })?;
        entry
            .descriptor
            .take()
            .ok_or(DescriptorError::AlreadyConsumed { slot })
    }

    /// Returns the number of descriptors not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.descriptor.is_some())
            .count()
    }
}

/// Descriptor manifest or validation failure.
#[derive(Debug, Error)]
pub enum DescriptorError {
    /// Manifest exceeds the transport descriptor limit.
    #[error("manifest has {actual} slots; maximum is {maximum}")]
    ManifestTooLarge { actual: usize, maximum: usize },
    /// Slot ID appears more than once.
    #[error("descriptor slot {slot:?} is duplicated")]
    DuplicateSlot { slot: SlotId },
    /// Bind identity does not match the descriptor kind.
    #[error("descriptor slot {slot:?} has an incompatible bind identity")]
    IncompatibleBind { slot: SlotId },
    /// Unix mode metadata is present on a non-filesystem-Unix listener.
    #[error("descriptor slot {slot:?} has incompatible Unix mode metadata")]
    IncompatibleMode { slot: SlotId },
    /// Received descriptor count differs from the manifest.
    #[error("manifest expects {expected} descriptors, but received {actual}")]
    CardinalityMismatch { expected: usize, actual: usize },
    /// Slot does not exist.
    #[error("descriptor slot {slot:?} is unknown")]
    UnknownSlot { slot: SlotId },
    /// Slot ownership was already consumed.
    #[error("descriptor slot {slot:?} was already consumed")]
    AlreadyConsumed { slot: SlotId },
    /// Descriptor is not a socket according to `fstat`.
    #[error("descriptor slot {slot:?} is not a socket")]
    NotSocket { slot: SlotId },
    /// Descriptor is not a stream socket.
    #[error("descriptor slot {slot:?} is not a stream socket")]
    NotStream { slot: SlotId },
    /// Descriptor is not a datagram socket.
    #[error("descriptor slot {slot:?} is not a datagram socket")]
    NotDatagram { slot: SlotId },
    /// Descriptor is not in listening state.
    #[error("descriptor slot {slot:?} is not listening")]
    NotListening { slot: SlotId },
    /// Descriptor could not be confirmed nonblocking after setting `O_NONBLOCK`.
    #[error("descriptor slot {slot:?} could not be made nonblocking")]
    NonblockingUnavailable { slot: SlotId },
    /// Descriptor could not be made close-on-exec.
    #[error("descriptor slot {slot:?} could not be made close-on-exec")]
    CloexecUnavailable { slot: SlotId },
    /// Descriptor address family does not match its kind.
    #[error("descriptor slot {slot:?} has the wrong address family")]
    WrongAddressFamily { slot: SlotId },
    /// Descriptor local bind does not match the expected identity.
    #[error("descriptor slot {slot:?} bind identity does not match")]
    BindMismatch { slot: SlotId },
    /// Filesystem Unix socket mode does not match the manifest.
    #[error("descriptor slot {slot:?} Unix mode does not match")]
    ModeMismatch { slot: SlotId },
    /// Filesystem Unix socket metadata could not be inspected.
    #[error("descriptor slot {slot:?} Unix path inspection failed: {source}")]
    InspectPath {
        slot: SlotId,
        #[source]
        source: std::io::Error,
    },
    /// Operating-system descriptor inspection failed.
    #[error("descriptor slot {slot:?} inspection failed: {source}")]
    Inspect {
        slot: SlotId,
        #[source]
        source: rustix::io::Errno,
    },
}

fn inspect<T>(slot: SlotId, result: rustix::io::Result<T>) -> Result<T, DescriptorError> {
    result.map_err(|source| DescriptorError::Inspect { slot, source })
}

fn validate_descriptor(
    slot: &DescriptorSlot,
    descriptor: &impl AsFd,
) -> Result<(), DescriptorError> {
    if slot.kind == DescriptorKind::Opaque {
        return Ok(());
    }
    let status_flags = inspect(slot.id, fcntl_getfl(descriptor))?;
    if !status_flags.contains(OFlags::NONBLOCK) {
        inspect(
            slot.id,
            rustix::fs::fcntl_setfl(descriptor, status_flags | OFlags::NONBLOCK),
        )?;
    }
    if !inspect(slot.id, fcntl_getfl(descriptor))?.contains(OFlags::NONBLOCK) {
        return Err(DescriptorError::NonblockingUnavailable { slot: slot.id });
    }
    let stat = inspect(slot.id, fstat(descriptor))?;
    if !FileType::from_raw_mode(stat.st_mode).is_socket() {
        return Err(DescriptorError::NotSocket { slot: slot.id });
    }
    validate_socket_shape(slot, descriptor)?;
    let address = inspect(slot.id, getsockname(descriptor))?;
    validate_socket_address(slot, address)
}

fn validate_socket_shape(
    slot: &DescriptorSlot,
    descriptor: &impl AsFd,
) -> Result<(), DescriptorError> {
    let socket_type = inspect(slot.id, sockopt::socket_type(descriptor))?;
    match slot.kind {
        DescriptorKind::TcpListener | DescriptorKind::UnixListener => {
            if socket_type != SocketType::STREAM {
                return Err(DescriptorError::NotStream { slot: slot.id });
            }
            if !inspect(slot.id, sockopt::socket_acceptconn(descriptor))? {
                return Err(DescriptorError::NotListening { slot: slot.id });
            }
        }
        DescriptorKind::DatagramListener | DescriptorKind::QuicListener => {
            if socket_type != SocketType::DGRAM {
                return Err(DescriptorError::NotDatagram { slot: slot.id });
            }
        }
        DescriptorKind::Opaque => unreachable!("opaque descriptors return before validation"),
    }
    Ok(())
}

fn validate_socket_address(
    slot: &DescriptorSlot,
    address: rustix::net::SocketAddrAny,
) -> Result<(), DescriptorError> {
    match slot.kind {
        DescriptorKind::TcpListener
        | DescriptorKind::DatagramListener
        | DescriptorKind::QuicListener => {
            validate_inet_bind(slot, address)?;
        }
        DescriptorKind::UnixListener => {
            validate_unix_bind(slot, address)?;
        }
        DescriptorKind::Opaque => unreachable!("opaque descriptors return before validation"),
    }
    Ok(())
}

fn validate_inet_bind(
    slot: &DescriptorSlot,
    address: rustix::net::SocketAddrAny,
) -> Result<(), DescriptorError> {
    if !matches!(
        address.address_family(),
        AddressFamily::INET | AddressFamily::INET6
    ) {
        return Err(DescriptorError::WrongAddressFamily { slot: slot.id });
    }
    if let Some(BindIdentity::Tcp(expected)) = &slot.bind {
        let actual = SocketAddr::try_from(address).map_err(|source| DescriptorError::Inspect {
            slot: slot.id,
            source,
        })?;
        if &actual != expected {
            return Err(DescriptorError::BindMismatch { slot: slot.id });
        }
    }
    Ok(())
}

fn validate_unix_bind(
    slot: &DescriptorSlot,
    address: rustix::net::SocketAddrAny,
) -> Result<(), DescriptorError> {
    if address.address_family() != AddressFamily::UNIX {
        return Err(DescriptorError::WrongAddressFamily { slot: slot.id });
    }
    let actual = SocketAddrUnix::try_from(address).map_err(|source| DescriptorError::Inspect {
        slot: slot.id,
        source,
    })?;
    let matches = match &slot.bind {
        None => true,
        Some(BindIdentity::UnixPath(expected)) => {
            actual.path_bytes() == Some(expected.as_os_str().as_bytes())
        }
        Some(BindIdentity::UnixAbstract(expected)) => {
            actual.abstract_name() == Some(expected.as_slice())
        }
        Some(BindIdentity::Tcp(_)) => false,
    };
    if !matches {
        return Err(DescriptorError::BindMismatch { slot: slot.id });
    }
    if let (Some(BindIdentity::UnixPath(path)), Some(expected)) = (&slot.bind, slot.mode) {
        use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

        let metadata =
            std::fs::symlink_metadata(path).map_err(|source| DescriptorError::InspectPath {
                slot: slot.id,
                source,
            })?;
        if !metadata.file_type().is_socket()
            || metadata.permissions().mode() & 0o7777 != u32::from(expected)
        {
            return Err(DescriptorError::ModeMismatch { slot: slot.id });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(id: u16, name: &str, address: &str) -> DescriptorSlot {
        DescriptorSlot {
            id: SlotId(id),
            role: DescriptorRole::Traffic(name.into()),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(address.parse().expect("socket address"))),
            mode: None,
        }
    }

    fn unix(id: u16, name: &str, path: &str, mode: Option<u16>) -> DescriptorSlot {
        DescriptorSlot {
            id: SlotId(id),
            role: DescriptorRole::Traffic(name.into()),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(path.into())),
            mode,
        }
    }

    fn manifest(slots: Vec<DescriptorSlot>) -> DescriptorManifest {
        DescriptorManifest::new(slots).expect("valid manifest")
    }

    #[test]
    fn identical_manifests_keep_every_socket() {
        let current = manifest(vec![
            tcp(0, "web", "127.0.0.1:8080"),
            tcp(1, "api", "127.0.0.1:8081"),
        ]);
        let diff = current.diff(&current.clone());
        assert!(diff.is_unchanged());
        assert_eq!(diff.kept, [(SlotId(0), SlotId(0)), (SlotId(1), SlotId(1))]);
    }

    #[test]
    fn an_added_listener_is_reported_with_its_next_slot() {
        let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
        let next = manifest(vec![
            tcp(0, "web", "127.0.0.1:8080"),
            tcp(1, "analysis", "127.0.0.1:8195"),
        ]);
        let diff = current.diff(&next);
        assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
        assert_eq!(diff.added, [SlotId(1)]);
        assert!(diff.removed.is_empty());
        assert!(!diff.is_unchanged());
    }

    #[test]
    fn a_removed_listener_is_reported_with_its_current_slot() {
        let current = manifest(vec![
            tcp(0, "web", "127.0.0.1:8080"),
            tcp(1, "analysis", "127.0.0.1:8195"),
        ]);
        let next = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
        let diff = current.diff(&next);
        assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
        assert_eq!(diff.removed, [SlotId(1)]);
        assert!(diff.added.is_empty());
    }

    #[test]
    fn a_changed_address_is_one_removal_and_one_addition() {
        let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
        let next = manifest(vec![tcp(0, "web", "127.0.0.1:9090")]);
        let diff = current.diff(&next);
        assert!(diff.kept.is_empty());
        assert_eq!(diff.removed, [SlotId(0)]);
        assert_eq!(diff.added, [SlotId(0)]);
    }

    #[test]
    fn a_renamed_or_renumbered_listener_keeps_its_socket() {
        let current = manifest(vec![
            tcp(0, "web", "127.0.0.1:8080"),
            tcp(1, "api", "127.0.0.1:8081"),
        ]);
        let next = manifest(vec![
            tcp(0, "api-renamed", "127.0.0.1:8081"),
            tcp(1, "web", "127.0.0.1:8080"),
        ]);
        let diff = current.diff(&next);
        assert!(diff.is_unchanged());
        assert_eq!(diff.kept, [(SlotId(0), SlotId(1)), (SlotId(1), SlotId(0))]);
    }

    #[test]
    fn the_same_address_with_another_socket_kind_is_a_different_socket() {
        let current = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
        let mut udp = tcp(0, "web", "127.0.0.1:8080");
        udp.kind = DescriptorKind::DatagramListener;
        let diff = current.diff(&manifest(vec![udp]));
        assert!(diff.kept.is_empty());
        assert_eq!(diff.removed, [SlotId(0)]);
        assert_eq!(diff.added, [SlotId(0)]);
    }

    #[test]
    fn a_unix_socket_matches_by_path_and_mode() {
        let current = manifest(vec![unix(0, "local", "/run/a.sock", Some(0o660))]);
        let same = manifest(vec![unix(5, "local", "/run/a.sock", Some(0o660))]);
        assert_eq!(current.diff(&same).kept, [(SlotId(0), SlotId(5))]);
        let other_mode = manifest(vec![unix(0, "local", "/run/a.sock", Some(0o600))]);
        let diff = current.diff(&other_mode);
        assert!(diff.kept.is_empty());
        assert_eq!(diff.removed, [SlotId(0)]);
        assert_eq!(diff.added, [SlotId(0)]);
        let other_path = manifest(vec![unix(0, "local", "/run/b.sock", Some(0o660))]);
        assert!(current.diff(&other_path).kept.is_empty());
    }

    #[test]
    fn repeated_identities_pair_one_to_one() {
        let current = manifest(vec![
            tcp(0, "a", "127.0.0.1:8080"),
            tcp(1, "b", "127.0.0.1:8080"),
        ]);
        let next = manifest(vec![tcp(0, "a", "127.0.0.1:8080")]);
        let diff = current.diff(&next);
        assert_eq!(diff.kept, [(SlotId(0), SlotId(0))]);
        assert_eq!(diff.removed, [SlotId(1)]);
    }

    #[test]
    fn slots_without_a_bind_match_by_role() {
        let opaque = |id, role| DescriptorSlot {
            id: SlotId(id),
            role,
            kind: DescriptorKind::Opaque,
            bind: None,
            mode: None,
        };
        let current = manifest(vec![opaque(0, DescriptorRole::State)]);
        assert!(
            current
                .diff(&manifest(vec![opaque(3, DescriptorRole::State)]))
                .is_unchanged()
        );
        assert!(
            !current
                .diff(&manifest(vec![opaque(0, DescriptorRole::Control)]))
                .is_unchanged()
        );
    }

    #[test]
    fn changes_name_listeners_for_operators() {
        let current = manifest(vec![
            tcp(0, "web", "127.0.0.1:8080"),
            tcp(1, "old", "127.0.0.1:8081"),
            unix(2, "local", "/run/a.sock", None),
        ]);
        let next = manifest(vec![
            tcp(0, "web-renamed", "127.0.0.1:8080"),
            tcp(1, "analysis", "127.0.0.1:8195"),
            unix(2, "local", "/run/a.sock", None),
        ]);
        let changes = current.changes(&next);
        assert_eq!(changes.added, ["analysis (tcp 127.0.0.1:8195)"]);
        assert_eq!(changes.removed, ["old (tcp 127.0.0.1:8081)"]);
        assert_eq!(
            changes.kept,
            [
                "web-renamed (tcp 127.0.0.1:8080)",
                "local (unix /run/a.sock)"
            ]
        );
        assert!(changes.changes_listeners());
        assert!(!current.changes(&current.clone()).changes_listeners());
    }

    #[test]
    fn labels_cover_every_role_and_bind_shape() {
        let slot = |role, kind, bind| DescriptorSlot {
            id: SlotId(0),
            role,
            kind,
            bind,
            mode: None,
        };
        let address = || Some(BindIdentity::Tcp("[::1]:9".parse().unwrap()));
        assert_eq!(
            slot(
                DescriptorRole::Management,
                DescriptorKind::TcpListener,
                address()
            )
            .label(),
            "@management (tcp [::1]:9)"
        );
        assert_eq!(
            slot(
                DescriptorRole::Stats(1),
                DescriptorKind::DatagramListener,
                address()
            )
            .label(),
            "@stats-1 (udp [::1]:9)"
        );
        assert_eq!(
            slot(
                DescriptorRole::StatsPage(2),
                DescriptorKind::QuicListener,
                address()
            )
            .label(),
            "@stats-page-2 (quic [::1]:9)"
        );
        assert_eq!(
            slot(
                DescriptorRole::Traffic("a".into()),
                DescriptorKind::UnixListener,
                Some(BindIdentity::UnixAbstract(b"name".to_vec())),
            )
            .label(),
            "a (unix @name)"
        );
        assert_eq!(
            slot(DescriptorRole::State, DescriptorKind::Opaque, None).label(),
            "@state"
        );
        assert_eq!(
            slot(DescriptorRole::Control, DescriptorKind::Opaque, None).label(),
            "@control"
        );
    }

    #[test]
    fn an_empty_manifest_adds_and_removes_everything() {
        let empty = manifest(Vec::new());
        let one = manifest(vec![tcp(0, "web", "127.0.0.1:8080")]);
        assert_eq!(empty.diff(&one).added, [SlotId(0)]);
        assert_eq!(one.diff(&empty).removed, [SlotId(0)]);
        assert!(empty.diff(&empty).is_unchanged());
    }
}
