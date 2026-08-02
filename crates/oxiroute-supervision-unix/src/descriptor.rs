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
    net::{AddressFamily, SocketAddrUnix, SocketType, getsockname, sockopt},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::MAX_DESCRIPTOR_COUNT;

/// Stable identifier for one descriptor slot in a manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SlotId(pub u16);

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
    /// No listener-specific validation beyond manifest ownership.
    Opaque,
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
                    | (DescriptorKind::TcpListener, Some(BindIdentity::Tcp(_)))
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
    /// Descriptor is not in listening state.
    #[error("descriptor slot {slot:?} is not listening")]
    NotListening { slot: SlotId },
    /// Descriptor could not be confirmed nonblocking after setting `O_NONBLOCK`.
    #[error("descriptor slot {slot:?} could not be made nonblocking")]
    NonblockingUnavailable { slot: SlotId },
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
    if inspect(slot.id, sockopt::socket_type(descriptor))? != SocketType::STREAM {
        return Err(DescriptorError::NotStream { slot: slot.id });
    }
    if !inspect(slot.id, sockopt::socket_acceptconn(descriptor))? {
        return Err(DescriptorError::NotListening { slot: slot.id });
    }
    let address = inspect(slot.id, getsockname(descriptor))?;
    match slot.kind {
        DescriptorKind::TcpListener => {
            if !matches!(
                address.address_family(),
                AddressFamily::INET | AddressFamily::INET6
            ) {
                return Err(DescriptorError::WrongAddressFamily { slot: slot.id });
            }
            if let Some(BindIdentity::Tcp(expected)) = &slot.bind {
                let actual =
                    SocketAddr::try_from(address).map_err(|source| DescriptorError::Inspect {
                        slot: slot.id,
                        source,
                    })?;
                if &actual != expected {
                    return Err(DescriptorError::BindMismatch { slot: slot.id });
                }
            }
        }
        DescriptorKind::UnixListener => {
            if address.address_family() != AddressFamily::UNIX {
                return Err(DescriptorError::WrongAddressFamily { slot: slot.id });
            }
            let actual =
                SocketAddrUnix::try_from(address).map_err(|source| DescriptorError::Inspect {
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

                let metadata = std::fs::symlink_metadata(path).map_err(|source| {
                    DescriptorError::InspectPath {
                        slot: slot.id,
                        source,
                    }
                })?;
                if !metadata.file_type().is_socket()
                    || metadata.permissions().mode() & 0o7777 != u32::from(expected)
                {
                    return Err(DescriptorError::ModeMismatch { slot: slot.id });
                }
            }
        }
        DescriptorKind::Opaque => unreachable!("opaque descriptors return before validation"),
    }
    Ok(())
}
