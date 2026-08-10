use std::{
    ffi::OsString,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    os::{
        fd::OwnedFd,
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::PathBuf,
};

use oxiroute_supervision::{BoundedWireProtocol, BoundedWireReader, BoundedWireWriter, Sequence};
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorCapabilities, DescriptorError, DescriptorKind, DescriptorManifest,
    DescriptorRole, DescriptorSet, DescriptorSlot, FrameFlags, MAX_DESCRIPTOR_COUNT, MessageType,
    SlotId,
};
use oxiroute_supervisor_process::{
    AuthenticatedChannelError, AuthenticatedFrame, ChildHandshakeError, WorkerEndpoint,
    WorkerIdentity,
};
use thiserror::Error;

use crate::status::{STATUS_MESSAGE, StatusProtocolError, WorkerStatus, encode_status};

/// Application protocol version used in the authenticated worker identity and every payload.
pub const CONTROL_PROTOCOL_VERSION: u16 = 2;
/// Maximum binary manifest bytes accepted by either endpoint.
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;
/// Version of the typed descriptor manifest payload.
pub const DESCRIPTOR_MANIFEST_VERSION: u16 = 1;
/// Descriptor capabilities implemented by this worker protocol.
pub const SUPPORTED_DESCRIPTOR_CAPABILITIES: DescriptorCapabilities = DescriptorCapabilities::ALL;

const ADOPT: MessageType = MessageType(0x100);
const QUIESCE: MessageType = MessageType(0x101);
const ACTIVATE: MessageType = MessageType(0x102);
const DRAIN: MessageType = MessageType(0x103);
const REACTIVATE: MessageType = MessageType(0x104);
const SHUTDOWN: MessageType = MessageType(0x105);
pub(crate) const ACK: MessageType = MessageType(0x180);
const PREFIX_SIZE: usize = 10;
const ACK_SIZE: usize = 12;

/// Correlated control phase represented on the wire by a fixed tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlPhase {
    /// Receive and validate listener duplicates.
    AdoptListeners,
    /// Stop admitting new work on the active worker.
    Quiesce,
    /// Start serving from the prepared worker.
    Activate,
    /// Finish admitted work on the retired worker.
    Drain,
    /// Resume the old active worker during rollback.
    Reactivate,
    /// Exit and allow the launcher to reap the worker group.
    Shutdown,
}

impl ControlPhase {
    const fn tag(self) -> u8 {
        match self {
            Self::AdoptListeners => 1,
            Self::Quiesce => 2,
            Self::Activate => 3,
            Self::Drain => 4,
            Self::Reactivate => 5,
            Self::Shutdown => 6,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, ControlProtocolError> {
        match tag {
            1 => Ok(Self::AdoptListeners),
            2 => Ok(Self::Quiesce),
            3 => Ok(Self::Activate),
            4 => Ok(Self::Drain),
            5 => Ok(Self::Reactivate),
            6 => Ok(Self::Shutdown),
            _ => Err(ControlProtocolError::UnknownPhase(tag)),
        }
    }

    pub(crate) const fn message_type(self) -> MessageType {
        match self {
            Self::AdoptListeners => ADOPT,
            Self::Quiesce => QUIESCE,
            Self::Activate => ACTIVATE,
            Self::Drain => DRAIN,
            Self::Reactivate => REACTIVATE,
            Self::Shutdown => SHUTDOWN,
        }
    }

    fn from_message_type(message_type: MessageType) -> Result<Self, ControlProtocolError> {
        match message_type {
            ADOPT => Ok(Self::AdoptListeners),
            QUIESCE => Ok(Self::Quiesce),
            ACTIVATE => Ok(Self::Activate),
            DRAIN => Ok(Self::Drain),
            REACTIVATE => Ok(Self::Reactivate),
            SHUTDOWN => Ok(Self::Shutdown),
            _ => Err(ControlProtocolError::UnexpectedMessage(message_type.0)),
        }
    }
}

/// Bounded worker acknowledgement outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlOutcome {
    /// The phase completed.
    Accepted,
    /// The worker rejected the phase without an unbounded error string.
    Rejected(u8),
}

/// One typed request received by a worker.
#[derive(Debug)]
pub struct ControlRequest {
    request_id: u64,
    phase: ControlPhase,
    listeners: Option<DescriptorSet>,
}

impl ControlRequest {
    /// Returns the correlation identity assigned by the master.
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }

    /// Returns the requested phase.
    #[must_use]
    pub const fn phase(&self) -> ControlPhase {
        self.phase
    }

    /// Takes the validated listener set from an adoption request.
    #[must_use]
    pub fn take_listeners(&mut self) -> Option<DescriptorSet> {
        self.listeners.take()
    }
}

/// Worker-side typed access to the authenticated process channel.
#[derive(Debug)]
pub struct WorkerControl {
    endpoint: WorkerEndpoint,
}

impl WorkerControl {
    /// Authenticates the parent and announces process readiness before application threads start.
    ///
    /// # Errors
    ///
    /// Returns the underlying child handshake failure.
    pub fn adopt_at_process_entry(identity: WorkerIdentity) -> Result<Self, ChildHandshakeError> {
        WorkerEndpoint::adopt_at_process_entry(identity).map(|endpoint| Self { endpoint })
    }

    /// Blocks for and decodes one bounded typed request.
    ///
    /// # Errors
    ///
    /// Returns an error for authentication, framing, version, shape, or descriptor validation
    /// failures.
    pub fn receive(&mut self) -> Result<ControlRequest, ControlProtocolError> {
        decode_request(self.endpoint.receive()?)
    }

    /// Decodes one bounded typed request only when the channel is immediately readable.
    ///
    /// # Errors
    ///
    /// Returns an error for authentication, polling, framing, version, shape, or descriptor
    /// validation failures.
    pub fn try_receive(&mut self) -> Result<Option<ControlRequest>, ControlProtocolError> {
        self.endpoint.try_receive()?.map(decode_request).transpose()
    }

    /// Acknowledges a request using its exact correlation identity and phase.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the bounded frame cannot be sent.
    pub fn acknowledge(
        &mut self,
        request: &ControlRequest,
        outcome: ControlOutcome,
    ) -> Result<Sequence, ControlProtocolError> {
        self.acknowledge_raw(request.request_id, request.phase, outcome)
    }

    /// Sends a raw acknowledgement, primarily for protocol conformance fixtures.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the bounded frame cannot be sent.
    pub fn acknowledge_raw(
        &mut self,
        request_id: u64,
        phase: ControlPhase,
        outcome: ControlOutcome,
    ) -> Result<Sequence, ControlProtocolError> {
        let payload = encode_ack(request_id, phase, outcome);
        Ok(self
            .endpoint
            .send(ACK, FrameFlags::default(), &payload, &[])?)
    }

    /// Sends one authenticated worker status observation to the master.
    ///
    /// Status observations are bounded and cannot request a worker-side action.
    ///
    /// # Errors
    ///
    /// Returns a protocol or transport error when the status cannot be encoded or sent.
    pub fn report_status(
        &mut self,
        status: &WorkerStatus,
    ) -> Result<Sequence, ControlProtocolError> {
        let payload = encode_status(status).map_err(ControlProtocolError::Status)?;
        Ok(self
            .endpoint
            .send(STATUS_MESSAGE, FrameFlags::default(), &payload, &[])?)
    }

    /// Sends a raw status payload for protocol conformance fixtures.
    ///
    /// Production workers should use [`Self::report_status`] so application bounds are enforced
    /// before the authenticated frame is sent.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the bounded frame cannot be sent.
    pub fn report_status_raw(&mut self, payload: &[u8]) -> Result<Sequence, ControlProtocolError> {
        Ok(self
            .endpoint
            .send(STATUS_MESSAGE, FrameFlags::default(), payload, &[])?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ControlAck {
    pub request_id: u64,
    pub phase: ControlPhase,
    pub outcome: ControlOutcome,
}

pub(crate) fn encode_request(request_id: u64, phase: ControlPhase) -> [u8; PREFIX_SIZE] {
    let mut payload = [0_u8; PREFIX_SIZE];
    payload[..2].copy_from_slice(&CONTROL_PROTOCOL_VERSION.to_be_bytes());
    payload[2..].copy_from_slice(&request_id.to_be_bytes());
    debug_assert_ne!(phase.message_type(), ADOPT);
    payload
}

pub(crate) fn encode_adopt_request(
    request_id: u64,
    manifest: &DescriptorManifest,
) -> Result<Vec<u8>, ControlProtocolError> {
    let mut payload = BoundedEncoder::new(PREFIX_SIZE + MAX_MANIFEST_BYTES);
    payload.bytes(&CONTROL_PROTOCOL_VERSION.to_be_bytes())?;
    payload.bytes(&request_id.to_be_bytes())?;
    encode_manifest(&mut payload, manifest)?;
    Ok(payload.finish())
}

pub(crate) fn decode_ack(frame: &AuthenticatedFrame) -> Result<ControlAck, ControlProtocolError> {
    if frame.header().message_type() != ACK {
        return Err(ControlProtocolError::UnexpectedMessage(
            frame.header().message_type().0,
        ));
    }
    if !frame.descriptors().is_empty() {
        return Err(ControlProtocolError::UnexpectedDescriptors);
    }
    let payload = frame.payload();
    if payload.len() != ACK_SIZE {
        return Err(ControlProtocolError::InvalidPayload);
    }
    check_version(payload)?;
    let outcome = match payload[11] {
        0 => ControlOutcome::Accepted,
        code => ControlOutcome::Rejected(code),
    };
    Ok(ControlAck {
        request_id: u64::from_be_bytes(payload[2..10].try_into().expect("fixed slice")),
        phase: ControlPhase::from_tag(payload[10])?,
        outcome,
    })
}

fn decode_request(frame: AuthenticatedFrame) -> Result<ControlRequest, ControlProtocolError> {
    let phase = ControlPhase::from_message_type(frame.header().message_type())?;
    let (header, payload, descriptors, _) = frame.into_parts();
    if payload.len() < PREFIX_SIZE {
        return Err(ControlProtocolError::InvalidPayload);
    }
    check_version(&payload)?;
    let request_id = u64::from_be_bytes(payload[2..10].try_into().expect("fixed slice"));
    let listeners = if phase == ControlPhase::AdoptListeners {
        decode_listeners(&payload[PREFIX_SIZE..], descriptors)?
    } else {
        if payload.len() != PREFIX_SIZE {
            return Err(ControlProtocolError::InvalidPayload);
        }
        if !descriptors.is_empty() {
            return Err(ControlProtocolError::UnexpectedDescriptors);
        }
        None
    };
    debug_assert_eq!(header.message_type(), phase.message_type());
    Ok(ControlRequest {
        request_id,
        phase,
        listeners,
    })
}

fn encode_manifest(
    encoder: &mut BoundedEncoder,
    manifest: &DescriptorManifest,
) -> Result<(), ControlProtocolError> {
    encoder.u16(DESCRIPTOR_MANIFEST_VERSION)?;
    encoder.u16(manifest.capabilities().bits())?;
    encoder.u16(
        u16::try_from(manifest.slots().len()).map_err(|_| ControlProtocolError::InvalidPayload)?,
    )?;
    for slot in manifest.slots() {
        encoder.u16(slot.id.0)?;
        match &slot.role {
            DescriptorRole::Traffic(name) => {
                encoder.u8(1)?;
                encoder.length_prefixed(name.as_bytes())?;
            }
            DescriptorRole::Management => encoder.u8(2)?,
            DescriptorRole::Stats(index) => {
                encoder.u8(3)?;
                encoder.u16(*index)?;
            }
            DescriptorRole::StatsPage(index) => {
                encoder.u8(4)?;
                encoder.u16(*index)?;
            }
            DescriptorRole::State => encoder.u8(5)?,
            DescriptorRole::Control => encoder.u8(6)?,
        }
        encoder.u8(match slot.kind {
            DescriptorKind::TcpListener => 1,
            DescriptorKind::UnixListener => 2,
            DescriptorKind::Opaque => 3,
            DescriptorKind::DatagramListener => 4,
            DescriptorKind::QuicListener => 5,
        })?;
        encode_bind(encoder, slot.bind.as_ref())?;
        match slot.mode {
            None => encoder.u8(0)?,
            Some(mode) => {
                encoder.u8(1)?;
                encoder.u16(mode)?;
            }
        }
    }
    Ok(())
}

fn encode_bind(
    encoder: &mut BoundedEncoder,
    bind: Option<&BindIdentity>,
) -> Result<(), ControlProtocolError> {
    match bind {
        None => encoder.u8(0),
        Some(BindIdentity::Tcp(SocketAddr::V4(address))) => {
            encoder.u8(1)?;
            encoder.bytes(&address.ip().octets())?;
            encoder.u16(address.port())
        }
        Some(BindIdentity::Tcp(SocketAddr::V6(address))) => {
            encoder.u8(2)?;
            encoder.bytes(&address.ip().octets())?;
            encoder.u16(address.port())?;
            encoder.u32(address.flowinfo())?;
            encoder.u32(address.scope_id())
        }
        Some(BindIdentity::UnixPath(path)) => {
            encoder.u8(3)?;
            encoder.length_prefixed(path.as_os_str().as_bytes())
        }
        Some(BindIdentity::UnixAbstract(name)) => {
            encoder.u8(4)?;
            encoder.length_prefixed(name)
        }
    }
}

fn decode_listeners(
    payload: &[u8],
    descriptors: Vec<OwnedFd>,
) -> Result<Option<DescriptorSet>, ControlProtocolError> {
    let manifest = decode_manifest(payload)?;
    Ok(Some(DescriptorSet::new(&manifest, descriptors)?))
}

fn decode_manifest(payload: &[u8]) -> Result<DescriptorManifest, ControlProtocolError> {
    if payload.len() > MAX_MANIFEST_BYTES {
        return Err(ControlProtocolError::ManifestTooLarge {
            actual: payload.len(),
            maximum: MAX_MANIFEST_BYTES,
        });
    }
    let mut decoder = Decoder::new(payload);
    let manifest_version = decoder.u16()?;
    if manifest_version != DESCRIPTOR_MANIFEST_VERSION {
        return Err(ControlProtocolError::ManifestVersionMismatch {
            expected: DESCRIPTOR_MANIFEST_VERSION,
            actual: manifest_version,
        });
    }
    let encoded_capabilities = decoder.u16()?;
    let capabilities = DescriptorCapabilities::from_bits(encoded_capabilities).ok_or(
        ControlProtocolError::UnsupportedCapabilities {
            required: encoded_capabilities,
            supported: SUPPORTED_DESCRIPTOR_CAPABILITIES.bits(),
        },
    )?;
    let slot_count = usize::from(decoder.u16()?);
    if slot_count > MAX_DESCRIPTOR_COUNT {
        return Err(ControlProtocolError::InvalidPayload);
    }
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(slot_count)
        .map_err(|_| ControlProtocolError::Allocation)?;
    for _ in 0..slot_count {
        let id = SlotId(decoder.u16()?);
        let role = match decoder.u8()? {
            1 => DescriptorRole::Traffic(
                std::str::from_utf8(decoder.length_prefixed()?)
                    .map_err(|_| ControlProtocolError::InvalidPayload)?
                    .to_owned(),
            ),
            2 => DescriptorRole::Management,
            3 => DescriptorRole::Stats(decoder.u16()?),
            4 => DescriptorRole::StatsPage(decoder.u16()?),
            5 => DescriptorRole::State,
            6 => DescriptorRole::Control,
            _ => return Err(ControlProtocolError::InvalidPayload),
        };
        let kind = match decoder.u8()? {
            1 => DescriptorKind::TcpListener,
            2 => DescriptorKind::UnixListener,
            3 => DescriptorKind::Opaque,
            4 => DescriptorKind::DatagramListener,
            5 => DescriptorKind::QuicListener,
            _ => return Err(ControlProtocolError::InvalidPayload),
        };
        let bind = decode_bind(&mut decoder)?;
        let mode = match decoder.u8()? {
            0 => None,
            1 => Some(decoder.u16()?),
            _ => return Err(ControlProtocolError::InvalidPayload),
        };
        slots.push(DescriptorSlot {
            id,
            role,
            kind,
            bind,
            mode,
        });
    }
    decoder.finish()?;
    let manifest = DescriptorManifest::new(slots)?;
    if manifest.capabilities() != capabilities {
        return Err(ControlProtocolError::InvalidPayload);
    }
    if !SUPPORTED_DESCRIPTOR_CAPABILITIES.contains(capabilities) {
        return Err(ControlProtocolError::UnsupportedCapabilities {
            required: capabilities.bits(),
            supported: SUPPORTED_DESCRIPTOR_CAPABILITIES.bits(),
        });
    }
    Ok(manifest)
}

fn decode_bind(decoder: &mut Decoder<'_>) -> Result<Option<BindIdentity>, ControlProtocolError> {
    match decoder.u8()? {
        0 => Ok(None),
        1 => {
            let ip = Ipv4Addr::from(<[u8; 4]>::try_from(decoder.bytes(4)?).expect("fixed slice"));
            let port = decoder.u16()?;
            Ok(Some(BindIdentity::Tcp(SocketAddr::V4(SocketAddrV4::new(
                ip, port,
            )))))
        }
        2 => {
            let ip = Ipv6Addr::from(<[u8; 16]>::try_from(decoder.bytes(16)?).expect("fixed slice"));
            let port = decoder.u16()?;
            let flowinfo = decoder.u32()?;
            let scope_id = decoder.u32()?;
            Ok(Some(BindIdentity::Tcp(SocketAddr::V6(SocketAddrV6::new(
                ip, port, flowinfo, scope_id,
            )))))
        }
        3 => Ok(Some(BindIdentity::UnixPath(PathBuf::from(
            OsString::from_vec(decoder.length_prefixed()?.to_vec()),
        )))),
        4 => Ok(Some(BindIdentity::UnixAbstract(
            decoder.length_prefixed()?.to_vec(),
        ))),
        _ => Err(ControlProtocolError::InvalidPayload),
    }
}

fn encode_ack(request_id: u64, phase: ControlPhase, outcome: ControlOutcome) -> [u8; ACK_SIZE] {
    let mut payload = [0_u8; ACK_SIZE];
    payload[..2].copy_from_slice(&CONTROL_PROTOCOL_VERSION.to_be_bytes());
    payload[2..10].copy_from_slice(&request_id.to_be_bytes());
    payload[10] = phase.tag();
    payload[11] = match outcome {
        ControlOutcome::Accepted => 0,
        ControlOutcome::Rejected(0) => 1,
        ControlOutcome::Rejected(code) => code,
    };
    payload
}

fn check_version(payload: &[u8]) -> Result<(), ControlProtocolError> {
    let actual = u16::from_be_bytes(payload[..2].try_into().expect("checked prefix"));
    if actual == CONTROL_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ControlProtocolError::VersionMismatch {
            expected: CONTROL_PROTOCOL_VERSION,
            actual,
        })
    }
}

struct ControlWire;

impl BoundedWireProtocol for ControlWire {
    type Error = ControlProtocolError;

    fn invalid() -> Self::Error {
        ControlProtocolError::InvalidPayload
    }

    fn too_large(actual: usize, maximum: usize) -> Self::Error {
        ControlProtocolError::ManifestTooLarge { actual, maximum }
    }

    fn allocation() -> Self::Error {
        ControlProtocolError::Allocation
    }
}

type BoundedEncoder = BoundedWireWriter<ControlWire>;
type Decoder<'a> = BoundedWireReader<'a, ControlWire>;

/// Typed control protocol failure.
#[derive(Debug, Error)]
pub enum ControlProtocolError {
    /// Child-side authenticated channel failure.
    #[error(transparent)]
    Channel(#[from] AuthenticatedChannelError),
    /// Child-side bounded transport send failure.
    #[error(transparent)]
    Transport(#[from] oxiroute_supervision_unix::TransportError),
    /// Descriptor validation failure.
    #[error(transparent)]
    Descriptor(#[from] DescriptorError),
    /// Payload protocol version did not match.
    #[error("control protocol version {actual} does not match {expected}")]
    VersionMismatch { expected: u16, actual: u16 },
    /// Typed descriptor manifest version did not match.
    #[error("descriptor manifest version {actual} does not match {expected}")]
    ManifestVersionMismatch { expected: u16, actual: u16 },
    /// Typed descriptor capabilities were not implemented by this worker.
    #[error("descriptor capabilities {required:#x} exceed worker capabilities {supported:#x}")]
    UnsupportedCapabilities { required: u16, supported: u16 },
    /// Message type is not part of this protocol.
    #[error("unexpected control message type {0}")]
    UnexpectedMessage(u16),
    /// Control phase tag is not defined.
    #[error("unknown control phase tag {0}")]
    UnknownPhase(u8),
    /// Payload did not have the exact required shape.
    #[error("control payload has an invalid shape")]
    InvalidPayload,
    /// A manifest exceeded its tighter application-level bound.
    #[error("encoded descriptor manifest has {actual} bytes; maximum is {maximum}")]
    ManifestTooLarge { actual: usize, maximum: usize },
    /// A bounded protocol allocation failed.
    #[error("bounded control protocol allocation failed")]
    Allocation,
    /// A non-adoption frame transferred descriptors.
    #[error("control message transferred unexpected descriptors")]
    UnexpectedDescriptors,
    /// A worker status observation failed its bounded application-level validation.
    #[error(transparent)]
    Status(#[from] StatusProtocolError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_manifest_round_trip_preserves_non_utf8_unix_path() {
        let path = PathBuf::from(OsString::from_vec(b"/tmp/master-\xff.sock".to_vec()));
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(9),
            role: DescriptorRole::Traffic(String::from("unix")),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(path)),
            mode: Some(0o660),
        }])
        .unwrap();
        let payload = encode_adopt_request(7, &manifest).unwrap();
        assert_eq!(decode_manifest(&payload[PREFIX_SIZE..]).unwrap(), manifest);
    }

    #[test]
    fn binary_manifest_rejects_oversize_before_append() {
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Traffic("x".repeat(MAX_MANIFEST_BYTES)),
            kind: DescriptorKind::Opaque,
            bind: None,
            mode: None,
        }])
        .unwrap();
        assert!(matches!(
            encode_adopt_request(1, &manifest),
            Err(ControlProtocolError::ManifestTooLarge { .. })
        ));
    }

    #[test]
    fn binary_manifest_rejects_slot_count_before_reserving() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&DESCRIPTOR_MANIFEST_VERSION.to_be_bytes());
        payload.extend_from_slice(&0_u16.to_be_bytes());
        payload.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(
            decode_manifest(&payload),
            Err(ControlProtocolError::InvalidPayload)
        ));
    }

    #[test]
    fn binary_manifest_rejects_an_unknown_manifest_version() {
        assert!(matches!(
            decode_manifest(&[0, 2, 0, 0, 0, 0]),
            Err(ControlProtocolError::ManifestVersionMismatch {
                expected: DESCRIPTOR_MANIFEST_VERSION,
                actual: 2,
            })
        ));
    }

    #[test]
    fn binary_manifest_rejects_capability_metadata_that_does_not_match_slots() {
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Traffic("udp".into()),
            kind: DescriptorKind::DatagramListener,
            bind: Some(BindIdentity::Tcp("127.0.0.1:9999".parse().unwrap())),
            mode: None,
        }])
        .unwrap();
        let mut payload = encode_adopt_request(1, &manifest).unwrap();
        payload[PREFIX_SIZE + 2..PREFIX_SIZE + 4]
            .copy_from_slice(&DescriptorCapabilities::STREAM.bits().to_be_bytes());
        assert!(matches!(
            decode_manifest(&payload[PREFIX_SIZE..]),
            Err(ControlProtocolError::InvalidPayload)
        ));
    }

    #[test]
    fn binary_manifest_accepts_quic_worker_support() {
        let manifest = DescriptorManifest::new(vec![DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Traffic("h3".into()),
            kind: DescriptorKind::QuicListener,
            bind: Some(BindIdentity::Tcp("127.0.0.1:9999".parse().unwrap())),
            mode: None,
        }])
        .unwrap();
        let payload = encode_adopt_request(1, &manifest).unwrap();

        assert_eq!(decode_manifest(&payload[PREFIX_SIZE..]).unwrap(), manifest);
    }

    #[test]
    fn version_two_control_decoder_rejects_legacy_version_one_payload() {
        assert!(matches!(
            check_version(&1_u16.to_be_bytes()),
            Err(ControlProtocolError::VersionMismatch {
                expected: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn control_prefix_and_ack_tags_stay_byte_exact() {
        let request_id = 0x0102_0304_0506_0708;
        assert_eq!(
            encode_request(request_id, ControlPhase::Quiesce),
            [0, 2, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            encode_ack(
                request_id,
                ControlPhase::Reactivate,
                ControlOutcome::Rejected(9)
            ),
            [0, 2, 1, 2, 3, 4, 5, 6, 7, 8, 5, 9]
        );
    }
}
