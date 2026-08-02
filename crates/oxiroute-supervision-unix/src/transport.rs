use std::{
    mem::MaybeUninit,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
};

use oxiroute_supervision::{GenerationId, Sequence};
use rustix::{
    cmsg_space,
    io::{FdFlags, IoSlice, IoSliceMut, fcntl_getfd, fcntl_setfd},
    net::{
        AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
        SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType, recvmsg,
        sendmsg, socketpair,
    },
};
use thiserror::Error;

use crate::PeerIdentity;

/// Maximum wire-frame size, including the fixed header.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;
/// Maximum number of descriptors carried by one frame.
pub const MAX_DESCRIPTOR_COUNT: usize = 64;
/// Fixed wire-header size.
pub const FRAME_HEADER_SIZE: usize = 52;
/// Maximum payload size after the fixed header.
pub const MAX_PAYLOAD_SIZE: usize = MAX_FRAME_SIZE - FRAME_HEADER_SIZE;

const MAGIC: [u8; 4] = *b"OXSP";
const VERSION: u16 = 1;

/// Application-defined supervision message type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MessageType(pub u16);

/// Application-defined frame flags.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FrameFlags(pub u32);

/// Fixed-width runtime instance identity carried in every frame header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InstanceToken(pub [u8; 16]);

/// Validated fixed frame header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    message_type: MessageType,
    payload_length: u32,
    flags: FrameFlags,
    descriptor_count: u16,
    sequence: Sequence,
    instance: InstanceToken,
    generation: GenerationId,
}

impl FrameHeader {
    /// Returns the message type.
    #[must_use]
    pub const fn message_type(self) -> MessageType {
        self.message_type
    }

    /// Returns the payload byte count.
    #[must_use]
    pub const fn payload_length(self) -> u32 {
        self.payload_length
    }

    /// Returns the application flags.
    #[must_use]
    pub const fn flags(self) -> FrameFlags {
        self.flags
    }

    /// Returns the declared descriptor count.
    #[must_use]
    pub const fn descriptor_count(self) -> u16 {
        self.descriptor_count
    }

    /// Returns the stream sequence.
    #[must_use]
    pub const fn sequence(self) -> Sequence {
        self.sequence
    }

    /// Returns the runtime instance token.
    #[must_use]
    pub const fn instance(self) -> InstanceToken {
        self.instance
    }

    /// Returns the service generation.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    fn encode(self) -> [u8; FRAME_HEADER_SIZE] {
        let mut bytes = [0_u8; FRAME_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_be_bytes());
        bytes[6..8].copy_from_slice(&self.message_type.0.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.payload_length.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.flags.0.to_be_bytes());
        bytes[16..18].copy_from_slice(&self.descriptor_count.to_be_bytes());
        bytes[18..20].copy_from_slice(&0_u16.to_be_bytes());
        bytes[20..28].copy_from_slice(&self.sequence.get().to_be_bytes());
        bytes[28..44].copy_from_slice(&self.instance.0);
        bytes[44..52].copy_from_slice(&self.generation.get().to_be_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, TransportError> {
        if bytes.len() < FRAME_HEADER_SIZE {
            return Err(TransportError::HeaderTruncated {
                actual: bytes.len(),
            });
        }
        if bytes[0..4] != MAGIC {
            return Err(TransportError::WrongMagic);
        }
        let version = u16::from_be_bytes(bytes[4..6].try_into().expect("fixed slice"));
        if version != VERSION {
            return Err(TransportError::WrongVersion {
                expected: VERSION,
                actual: version,
            });
        }
        let reserved = u16::from_be_bytes(bytes[18..20].try_into().expect("fixed slice"));
        if reserved != 0 {
            return Err(TransportError::ReservedBits);
        }
        Ok(Self {
            message_type: MessageType(u16::from_be_bytes(
                bytes[6..8].try_into().expect("fixed slice"),
            )),
            payload_length: u32::from_be_bytes(bytes[8..12].try_into().expect("fixed slice")),
            flags: FrameFlags(u32::from_be_bytes(
                bytes[12..16].try_into().expect("fixed slice"),
            )),
            descriptor_count: u16::from_be_bytes(bytes[16..18].try_into().expect("fixed slice")),
            sequence: Sequence(u64::from_be_bytes(
                bytes[20..28].try_into().expect("fixed slice"),
            )),
            instance: InstanceToken(bytes[28..44].try_into().expect("fixed slice")),
            generation: GenerationId(u64::from_be_bytes(
                bytes[44..52].try_into().expect("fixed slice"),
            )),
        })
    }
}

/// One complete seqpacket frame and its owned received descriptors.
#[derive(Debug)]
pub struct Frame {
    header: FrameHeader,
    payload: Vec<u8>,
    descriptors: Vec<OwnedFd>,
    peer_identity: PeerIdentity,
}

impl Frame {
    /// Returns the validated frame header.
    #[must_use]
    pub const fn header(&self) -> FrameHeader {
        self.header
    }

    /// Returns the exact payload bytes from this packet.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns the received descriptors in ancillary order.
    #[must_use]
    pub fn descriptors(&self) -> &[OwnedFd] {
        &self.descriptors
    }

    /// Returns the kernel-authenticated sender credentials attached to this packet.
    #[must_use]
    pub const fn peer_identity(&self) -> PeerIdentity {
        self.peer_identity
    }

    /// Separates payload metadata from descriptor ownership.
    #[must_use]
    pub fn into_parts(self) -> (FrameHeader, Vec<u8>, Vec<OwnedFd>, PeerIdentity) {
        (
            self.header,
            self.payload,
            self.descriptors,
            self.peer_identity,
        )
    }
}

/// Framing or transport failure.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Operating-system transport failure.
    #[error("Unix transport operation failed: {0}")]
    Io(#[from] rustix::io::Errno),
    /// The peer closed the socket.
    #[error("Unix transport peer closed the socket")]
    Closed,
    /// Payload exceeds the frame limit.
    #[error("payload is {actual} bytes; maximum is {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    /// Descriptor count exceeds the ancillary limit.
    #[error("descriptor count is {actual}; maximum is {maximum}")]
    TooManyDescriptors { actual: usize, maximum: usize },
    /// The kernel reported a partial packet send.
    #[error("seqpacket send wrote {actual} of {expected} bytes")]
    PartialSend { expected: usize, actual: usize },
    /// The data part of the packet did not fit the receive buffer.
    #[error("received frame exceeded {maximum} bytes")]
    PayloadTruncated { maximum: usize },
    /// Ancillary data did not fit the descriptor receive buffer.
    #[error("received ancillary data exceeded {maximum} descriptors")]
    AncillaryTruncated { maximum: usize },
    /// Frame ended before the fixed header.
    #[error("frame header is truncated at {actual} bytes")]
    HeaderTruncated { actual: usize },
    /// Header magic is not recognized.
    #[error("frame has wrong magic")]
    WrongMagic,
    /// Header version is unsupported.
    #[error("frame version {actual} does not match {expected}")]
    WrongVersion { expected: u16, actual: u16 },
    /// Reserved header bits were nonzero.
    #[error("frame reserved header bits are nonzero")]
    ReservedBits,
    /// Header length does not match the packet boundary.
    #[error("declared payload length is {declared}, but packet contains {actual} bytes")]
    LengthMismatch { declared: u32, actual: usize },
    /// Header descriptor count does not match ancillary data.
    #[error("declared descriptor count is {declared}, but packet contains {actual}")]
    DescriptorCountMismatch { declared: usize, actual: usize },
    /// Internal ancillary buffer could not encode a bounded descriptor list.
    #[error("bounded descriptor list did not fit its ancillary buffer")]
    AncillaryEncoding,
    /// Received sequence was not the exact next stream position.
    #[error("received sequence {actual}, but expected {expected}")]
    UnexpectedSequence {
        expected: Sequence,
        actual: Sequence,
    },
    /// No kernel-authenticated credentials accompanied the packet.
    #[error("frame is missing kernel-authenticated sender credentials")]
    MissingCredentials,
    /// More than one credential record accompanied the packet.
    #[error("frame contains multiple sender credential records")]
    MultipleCredentials,
    /// Sending another frame would overflow the sequence number.
    #[error("send sequence is exhausted")]
    SequenceExhausted,
}

/// One endpoint of a private Unix `SOCK_SEQPACKET` channel.
#[derive(Debug)]
pub struct SeqpacketEndpoint {
    fd: OwnedFd,
    next_send_sequence: u64,
    last_received_sequence: Option<Sequence>,
}

impl SeqpacketEndpoint {
    /// Creates a connected private `AF_UNIX` `SOCK_SEQPACKET` pair with `CLOEXEC` set.
    ///
    /// # Errors
    ///
    /// Returns the operating-system socket creation error.
    pub fn pair() -> Result<(Self, Self), TransportError> {
        let (first, second) = socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        Ok((Self::new(first)?, Self::new(second)?))
    }

    /// Adopts an already connected seqpacket descriptor and enables credential reception.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error from enabling `SO_PASSCRED`.
    pub fn from_owned_fd(fd: OwnedFd) -> Result<Self, TransportError> {
        Self::new(fd)
    }

    fn new(fd: OwnedFd) -> Result<Self, TransportError> {
        rustix::net::sockopt::set_socket_passcred(&fd, true)?;
        Ok(Self {
            fd,
            next_send_sequence: 1,
            last_received_sequence: None,
        })
    }

    /// Sends exactly one bounded packet and returns its assigned sequence.
    ///
    /// # Errors
    ///
    /// Returns an error for bounds violations, sequence exhaustion, OS failures, or a partial send.
    pub fn send(
        &mut self,
        message_type: MessageType,
        flags: FrameFlags,
        instance: InstanceToken,
        generation: GenerationId,
        payload: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<Sequence, TransportError> {
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(TransportError::PayloadTooLarge {
                actual: payload.len(),
                maximum: MAX_PAYLOAD_SIZE,
            });
        }
        if descriptors.len() > MAX_DESCRIPTOR_COUNT {
            return Err(TransportError::TooManyDescriptors {
                actual: descriptors.len(),
                maximum: MAX_DESCRIPTOR_COUNT,
            });
        }
        let sequence = Sequence(self.next_send_sequence);
        let next = self
            .next_send_sequence
            .checked_add(1)
            .ok_or(TransportError::SequenceExhausted)?;
        let payload_length =
            u32::try_from(payload.len()).map_err(|_| TransportError::PayloadTooLarge {
                actual: payload.len(),
                maximum: MAX_PAYLOAD_SIZE,
            })?;
        let descriptor_count =
            u16::try_from(descriptors.len()).map_err(|_| TransportError::TooManyDescriptors {
                actual: descriptors.len(),
                maximum: MAX_DESCRIPTOR_COUNT,
            })?;
        let header = FrameHeader {
            message_type,
            payload_length,
            flags,
            descriptor_count,
            sequence,
            instance,
            generation,
        }
        .encode();
        let iov = [IoSlice::new(&header), IoSlice::new(payload)];
        let mut ancillary_space =
            [MaybeUninit::uninit(); cmsg_space!(ScmRights(MAX_DESCRIPTOR_COUNT))];
        let mut ancillary = SendAncillaryBuffer::new(&mut ancillary_space);
        if !descriptors.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(descriptors))
        {
            return Err(TransportError::AncillaryEncoding);
        }
        let sent = sendmsg(&self.fd, &iov, &mut ancillary, SendFlags::NOSIGNAL)?;
        let expected = FRAME_HEADER_SIZE + payload.len();
        if sent != expected {
            return Err(TransportError::PartialSend {
                expected,
                actual: sent,
            });
        }
        self.next_send_sequence = next;
        Ok(sequence)
    }

    /// Receives and validates exactly one packet while preserving its boundary.
    ///
    /// Descriptors are atomically received with `CLOEXEC` and explicitly checked afterward. Every
    /// error path drops all descriptors received with the rejected packet.
    /// The safe rustix iterator exposes known `SCM_RIGHTS` and `SCM_CREDENTIALS` records and skips
    /// unknown control messages; this transport interprets the known records and does not claim
    /// strict rejection of unknown ancillary data.
    ///
    /// # Errors
    ///
    /// Returns an error for truncation, malformed headers, count mismatch, replay, or OS failure.
    pub fn receive(&mut self) -> Result<Frame, TransportError> {
        let mut bytes = vec![0_u8; MAX_FRAME_SIZE];
        let mut iov = [IoSliceMut::new(&mut bytes)];
        let mut ancillary_space = [MaybeUninit::uninit();
            cmsg_space!(ScmRights(MAX_DESCRIPTOR_COUNT), ScmCredentials(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut ancillary_space);
        let received = recvmsg(
            &self.fd,
            &mut iov,
            &mut ancillary,
            RecvFlags::TRUNC | RecvFlags::CMSG_CLOEXEC,
        )?;
        if received.bytes == 0 {
            return Err(TransportError::Closed);
        }

        let mut descriptors = Vec::new();
        let mut credentials = None;
        for message in ancillary.drain() {
            match message {
                RecvAncillaryMessage::ScmRights(received_fds) => descriptors.extend(received_fds),
                RecvAncillaryMessage::ScmCredentials(received) => {
                    if credentials.replace(received).is_some() {
                        return Err(TransportError::MultipleCredentials);
                    }
                }
                _ => {}
            }
        }
        if received.flags.contains(ReturnFlags::TRUNC) || received.bytes > MAX_FRAME_SIZE {
            return Err(TransportError::PayloadTruncated {
                maximum: MAX_FRAME_SIZE,
            });
        }
        if received.flags.contains(ReturnFlags::CTRUNC) {
            return Err(TransportError::AncillaryTruncated {
                maximum: MAX_DESCRIPTOR_COUNT,
            });
        }
        if descriptors.len() > MAX_DESCRIPTOR_COUNT {
            return Err(TransportError::AncillaryTruncated {
                maximum: MAX_DESCRIPTOR_COUNT,
            });
        }
        let peer_identity = credentials
            .map(PeerIdentity::from_credentials)
            .ok_or(TransportError::MissingCredentials)?;
        for descriptor in &descriptors {
            let flags = fcntl_getfd(descriptor)?;
            if !flags.contains(FdFlags::CLOEXEC) {
                fcntl_setfd(descriptor, flags | FdFlags::CLOEXEC)?;
            }
        }

        bytes.truncate(received.bytes);
        let header = FrameHeader::decode(&bytes)?;
        let payload = &bytes[FRAME_HEADER_SIZE..];
        let actual_length =
            u32::try_from(payload.len()).map_err(|_| TransportError::PayloadTruncated {
                maximum: MAX_FRAME_SIZE,
            })?;
        if header.payload_length != actual_length {
            return Err(TransportError::LengthMismatch {
                declared: header.payload_length,
                actual: payload.len(),
            });
        }
        let declared_descriptors = usize::from(header.descriptor_count);
        if declared_descriptors != descriptors.len() {
            return Err(TransportError::DescriptorCountMismatch {
                declared: declared_descriptors,
                actual: descriptors.len(),
            });
        }
        let expected_sequence = match self.last_received_sequence {
            Some(previous) => Sequence(
                previous
                    .get()
                    .checked_add(1)
                    .ok_or(TransportError::SequenceExhausted)?,
            ),
            None => Sequence(1),
        };
        if header.sequence != expected_sequence {
            return Err(TransportError::UnexpectedSequence {
                expected: expected_sequence,
                actual: header.sequence,
            });
        }
        self.last_received_sequence = Some(header.sequence);
        Ok(Frame {
            header,
            payload: payload.to_vec(),
            descriptors,
            peer_identity,
        })
    }

    /// Returns the last accepted incoming sequence, if any.
    #[must_use]
    pub const fn last_received_sequence(&self) -> Option<Sequence> {
        self.last_received_sequence
    }

    /// Consumes the endpoint and returns its owned socket descriptor.
    #[must_use]
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsFd for SeqpacketEndpoint {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
