use std::{
    fs,
    io::Read,
    mem::MaybeUninit,
    net::{TcpListener, TcpStream, UdpSocket},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::net::{UnixListener, UnixStream},
    },
    process::{Command, Stdio},
};

use oxiroute_supervision::GenerationId;
use oxiroute_supervision_unix::{
    BindIdentity, DescriptorCapabilities, DescriptorError, DescriptorKind, DescriptorManifest,
    DescriptorRole,
    DescriptorSet, DescriptorSlot, FRAME_HEADER_SIZE, FrameFlags, InstanceToken,
    MAX_DESCRIPTOR_COUNT, MAX_FRAME_SIZE, MAX_PAYLOAD_SIZE, MessageType, SeqpacketEndpoint, SlotId,
    SpawnHandshakeNonce, TransportError,
};
use rustix::{
    cmsg_space,
    io::{FdFlags, IoSlice, fcntl_getfd},
    net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg},
};

const INSTANCE: InstanceToken = InstanceToken(*b"worker-instance1");

fn wire_frame(
    sequence: u64,
    declared_payload_length: u32,
    declared_descriptors: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = vec![0_u8; FRAME_HEADER_SIZE];
    bytes[0..4].copy_from_slice(b"OXSP");
    bytes[4..6].copy_from_slice(&1_u16.to_be_bytes());
    bytes[6..8].copy_from_slice(&7_u16.to_be_bytes());
    bytes[8..12].copy_from_slice(&declared_payload_length.to_be_bytes());
    bytes[12..16].copy_from_slice(&9_u32.to_be_bytes());
    bytes[16..18].copy_from_slice(&declared_descriptors.to_be_bytes());
    bytes[20..28].copy_from_slice(&sequence.to_be_bytes());
    bytes[28..44].copy_from_slice(&INSTANCE.0);
    bytes[44..52].copy_from_slice(&3_u64.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn send_raw(endpoint: &SeqpacketEndpoint, bytes: &[u8], descriptors: &[BorrowedFd<'_>]) {
    let mut space = vec![MaybeUninit::uninit(); cmsg_space!(ScmRights(descriptors.len()))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !descriptors.is_empty() {
        assert!(ancillary.push(SendAncillaryMessage::ScmRights(descriptors)));
    }
    assert_eq!(
        sendmsg(
            endpoint,
            &[IoSlice::new(bytes)],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        )
        .unwrap(),
        bytes.len()
    );
}

fn assert_stream_eof(mut reader: &UnixStream) {
    let mut byte = [0_u8; 1];
    assert_eq!(reader.read(&mut byte).unwrap(), 0);
}

#[test]
fn packets_preserve_boundaries_and_fixed_header_metadata() {
    let (mut sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    assert_eq!(
        sender
            .send(
                MessageType(41),
                FrameFlags(0x55aa),
                INSTANCE,
                GenerationId(8),
                "first payload with spaces: olá".as_bytes(),
                &[],
            )
            .unwrap()
            .get(),
        1
    );
    sender
        .send(
            MessageType(42),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(9),
            b"second",
            &[],
        )
        .unwrap();

    let first = receiver.receive().unwrap();
    assert_eq!(first.header().message_type(), MessageType(41));
    assert_eq!(first.header().flags(), FrameFlags(0x55aa));
    assert_eq!(first.header().instance(), INSTANCE);
    assert_eq!(first.header().generation(), GenerationId(8));
    assert_eq!(first.payload(), "first payload with spaces: olá".as_bytes());
    assert_eq!(
        first.peer_identity().pid(),
        i32::try_from(std::process::id()).unwrap()
    );

    let second = receiver.receive().unwrap();
    assert_eq!(second.header().sequence().get(), 2);
    assert_eq!(second.payload(), b"second");
}

#[test]
fn payload_and_descriptor_send_bounds_are_enforced() {
    let (mut sender, _receiver) = SeqpacketEndpoint::pair().unwrap();
    let oversized = vec![0_u8; MAX_PAYLOAD_SIZE + 1];
    assert!(matches!(
        sender.send(
            MessageType(1),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(1),
            &oversized,
            &[],
        ),
        Err(TransportError::PayloadTooLarge { .. })
    ));

    let file = fs::File::open("/dev/null").unwrap();
    let descriptors = vec![file.as_fd(); MAX_DESCRIPTOR_COUNT + 1];
    assert!(matches!(
        sender.send(
            MessageType(1),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(1),
            b"fds",
            &descriptors,
        ),
        Err(TransportError::TooManyDescriptors { .. })
    ));
}

#[test]
fn transfers_tcp_and_unix_listeners_with_cloexec_and_consume_once_slots() {
    let temporary = tempfile::tempdir().unwrap();
    let unix_path = temporary.path().join("listener café with spaces.sock");
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let tcp_address = tcp.local_addr().unwrap();
    let unix = UnixListener::bind(&unix_path).unwrap();
    unix.set_nonblocking(true).unwrap();

    let manifest = DescriptorManifest::new(vec![
        DescriptorSlot {
            id: SlotId(11),
            role: DescriptorRole::Traffic(String::from("public café listener")),
            kind: DescriptorKind::TcpListener,
            bind: Some(BindIdentity::Tcp(tcp_address)),
            mode: None,
        },
        DescriptorSlot {
            id: SlotId(12),
            role: DescriptorRole::Traffic(String::from("local admin socket")),
            kind: DescriptorKind::UnixListener,
            bind: Some(BindIdentity::UnixPath(unix_path.clone())),
            mode: None,
        },
    ])
    .unwrap();
    let json = serde_json::to_string(&manifest).unwrap();
    assert!(json.contains("public café listener"));
    assert!(json.contains("listener café with spaces.sock"));
    assert_eq!(
        serde_json::from_str::<DescriptorManifest>(&json).unwrap(),
        manifest
    );

    let (mut sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    sender
        .send(
            MessageType(2),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(4),
            b"listener manifest",
            &[tcp.as_fd(), unix.as_fd()],
        )
        .unwrap();
    let (_, _, descriptors, peer_identity) = receiver.receive().unwrap().into_parts();
    assert_eq!(
        peer_identity.pid(),
        i32::try_from(std::process::id()).unwrap()
    );
    assert!(
        descriptors
            .iter()
            .all(|descriptor| { fcntl_getfd(descriptor).unwrap().contains(FdFlags::CLOEXEC) })
    );

    let mut set = DescriptorSet::new(&manifest, descriptors).unwrap();
    assert_eq!(set.remaining(), 2);
    assert_eq!(
        set.role(SlotId(11)),
        Some(&DescriptorRole::Traffic(String::from(
            "public café listener"
        )))
    );
    let transferred_tcp = TcpListener::from(set.take(SlotId(11)).unwrap());
    let transferred_unix = UnixListener::from(set.take(SlotId(12)).unwrap());
    assert!(matches!(
        set.take(SlotId(11)),
        Err(DescriptorError::AlreadyConsumed { .. })
    ));
    assert_eq!(set.remaining(), 0);

    let _tcp_client = TcpStream::connect(tcp_address).unwrap();
    loop {
        match transferred_tcp.accept() {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::yield_now();
            }
            Err(error) => panic!("TCP accept failed: {error}"),
        }
    }
    let _unix_client = UnixStream::connect(&unix_path).unwrap();
    loop {
        match transferred_unix.accept() {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::yield_now();
            }
            Err(error) => panic!("Unix accept failed: {error}"),
        }
    }
}

#[test]
fn transfers_typed_datagram_and_quic_listeners_with_exact_bind_identity() {
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_nonblocking(true).unwrap();
    let quic = UdpSocket::bind("127.0.0.1:0").unwrap();
    quic.set_nonblocking(true).unwrap();
    let udp_address = udp.local_addr().unwrap();
    let quic_address = quic.local_addr().unwrap();
    let manifest = DescriptorManifest::new(vec![
        DescriptorSlot {
            id: SlotId(21),
            role: DescriptorRole::Traffic("udp".into()),
            kind: DescriptorKind::DatagramListener,
            bind: Some(BindIdentity::Tcp(udp_address)),
            mode: None,
        },
        DescriptorSlot {
            id: SlotId(22),
            role: DescriptorRole::Traffic("quic".into()),
            kind: DescriptorKind::QuicListener,
            bind: Some(BindIdentity::Tcp(quic_address)),
            mode: None,
        },
    ])
    .unwrap();
    assert!(manifest.capabilities().contains(DescriptorCapabilities::DATAGRAM));
    assert!(manifest.capabilities().contains(DescriptorCapabilities::QUIC));

    let (mut sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    sender
        .send(
            MessageType(3),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(5),
            b"datagram manifest",
            &[udp.as_fd(), quic.as_fd()],
        )
        .unwrap();
    let descriptors = receiver.receive().unwrap().into_parts().2;
    let mut set = DescriptorSet::new(&manifest, descriptors).unwrap();
    let adopted_udp = UdpSocket::from(set.take(SlotId(21)).unwrap());
    let adopted_quic = UdpSocket::from(set.take(SlotId(22)).unwrap());
    assert_eq!(adopted_udp.local_addr().unwrap(), udp_address);
    assert_eq!(adopted_quic.local_addr().unwrap(), quic_address);
    assert_eq!(set.remaining(), 0);
}

#[test]
fn listener_validation_sets_nonblocking_and_rejects_wrong_bind() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&listener, 0).unwrap();
    let blocking_manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(1),
        role: DescriptorRole::Traffic(String::from("blocking")),
        kind: DescriptorKind::TcpListener,
        bind: None,
        mode: None,
    }])
    .unwrap();
    let mut set = DescriptorSet::new(&blocking_manifest, vec![duplicate]).unwrap();
    assert!(
        rustix::fs::fcntl_getfl(&listener)
            .unwrap()
            .contains(rustix::fs::OFlags::NONBLOCK)
    );
    drop(set.take(SlotId(1)).unwrap());

    listener.set_nonblocking(true).unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&listener, 0).unwrap();
    let wrong_bind = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(2),
        role: DescriptorRole::Traffic(String::from("wrong bind")),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp("127.0.0.1:1".parse().unwrap())),
        mode: None,
    }])
    .unwrap();
    assert!(matches!(
        DescriptorSet::new(&wrong_bind, vec![duplicate]),
        Err(DescriptorError::BindMismatch { .. })
    ));
}

#[test]
fn listener_validation_checks_fstat_socket_type_and_listening_state() {
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(3),
        role: DescriptorRole::Traffic(String::from("shape checks")),
        kind: DescriptorKind::TcpListener,
        bind: None,
        mode: None,
    }])
    .unwrap();

    let file = fs::File::open("/dev/null").unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&file, 0).unwrap();
    assert!(matches!(
        DescriptorSet::new(&manifest, vec![duplicate]),
        Err(DescriptorError::NotSocket { .. })
    ));

    let datagram = UdpSocket::bind("127.0.0.1:0").unwrap();
    datagram.set_nonblocking(true).unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&datagram, 0).unwrap();
    assert!(matches!(
        DescriptorSet::new(&manifest, vec![duplicate]),
        Err(DescriptorError::NotStream { .. })
    ));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    accepted.set_nonblocking(true).unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&accepted, 0).unwrap();
    assert!(matches!(
        DescriptorSet::new(&manifest, vec![duplicate]),
        Err(DescriptorError::NotListening { .. })
    ));
    drop(client);
}

#[test]
fn listener_validation_rejects_wrong_filesystem_unix_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("mode.sock");
    let listener = UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let duplicate = rustix::io::fcntl_dupfd_cloexec(&listener, 0).unwrap();
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(4),
        role: DescriptorRole::Traffic(String::from("mode")),
        kind: DescriptorKind::UnixListener,
        bind: Some(BindIdentity::UnixPath(path)),
        mode: Some(0o640),
    }])
    .unwrap();

    assert!(matches!(
        DescriptorSet::new(&manifest, vec![duplicate]),
        Err(DescriptorError::ModeMismatch { .. })
    ));
}

#[test]
fn malformed_headers_length_and_count_mismatches_are_rejected_without_fd_leaks() {
    let (sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    send_raw(&sender, b"short", &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::HeaderTruncated { .. })
    ));

    let mut wrong_magic = wire_frame(2, 0, 0, b"");
    wrong_magic[0] = b'X';
    send_raw(&sender, &wrong_magic, &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::WrongMagic)
    ));

    let mut wrong_version = wire_frame(3, 0, 0, b"");
    wrong_version[4..6].copy_from_slice(&2_u16.to_be_bytes());
    send_raw(&sender, &wrong_version, &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::WrongVersion { .. })
    ));

    send_raw(&sender, &wire_frame(4, 8, 0, b"tiny"), &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::LengthMismatch { .. })
    ));

    let (reader, writer) = UnixStream::pair().unwrap();
    send_raw(&sender, &wire_frame(5, 0, 0, b""), &[writer.as_fd()]);
    drop(writer);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::DescriptorCountMismatch { .. })
    ));
    assert_stream_eof(&reader);
}

#[test]
fn data_and_ancillary_truncation_are_rejected_and_close_received_fds() {
    let (sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    send_raw(&sender, &vec![0_u8; MAX_FRAME_SIZE + 1], &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::PayloadTruncated { .. })
    ));

    let (reader, writer) = UnixStream::pair().unwrap();
    let descriptors = vec![writer.as_fd(); MAX_DESCRIPTOR_COUNT + 1];
    send_raw(
        &sender,
        &wire_frame(2, 0, u16::try_from(descriptors.len()).unwrap(), b""),
        &descriptors,
    );
    drop(descriptors);
    drop(writer);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::AncillaryTruncated { .. })
    ));
    assert_stream_eof(&reader);
}

#[test]
fn sequence_must_start_at_one_and_remain_contiguous() {
    let (sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    send_raw(&sender, &wire_frame(9, 0, 0, b""), &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::UnexpectedSequence { .. })
    ));
    assert_eq!(receiver.last_received_sequence(), None);

    send_raw(&sender, &wire_frame(1, 0, 0, b""), &[]);
    assert_eq!(receiver.receive().unwrap().header().sequence().get(), 1);
    send_raw(&sender, &wire_frame(3, 0, 0, b""), &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::UnexpectedSequence { .. })
    ));
    assert_eq!(receiver.last_received_sequence().unwrap().get(), 1);

    let second = wire_frame(2, 0, 0, b"");
    send_raw(&sender, &second, &[]);
    assert_eq!(receiver.receive().unwrap().header().sequence().get(), 2);
    send_raw(&sender, &second, &[]);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::UnexpectedSequence { .. })
    ));
    assert_eq!(receiver.last_received_sequence().unwrap().get(), 2);
}

#[test]
fn manifests_enforce_unique_slots_and_exact_cardinality() {
    let duplicate = vec![
        DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::State,
            kind: DescriptorKind::Opaque,
            bind: None,
            mode: None,
        },
        DescriptorSlot {
            id: SlotId(1),
            role: DescriptorRole::Control,
            kind: DescriptorKind::Opaque,
            bind: None,
            mode: None,
        },
    ];
    assert!(matches!(
        DescriptorManifest::new(duplicate),
        Err(DescriptorError::DuplicateSlot { .. })
    ));

    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(1),
        role: DescriptorRole::State,
        kind: DescriptorKind::Opaque,
        bind: None,
        mode: None,
    }])
    .unwrap();
    assert!(matches!(
        DescriptorSet::new(&manifest, Vec::new()),
        Err(DescriptorError::CardinalityMismatch { .. })
    ));
}

#[test]
fn private_nonce_is_a_safe_redacted_value_interface() {
    let nonce = SpawnHandshakeNonce::new([0x5a; 32]);
    assert_eq!(nonce.as_bytes(), &[0x5a; 32]);
    assert_eq!(format!("{nonce:?}"), "SpawnHandshakeNonce([REDACTED])");
    let encoded = serde_json::to_string(&nonce).unwrap();
    assert_eq!(
        serde_json::from_str::<SpawnHandshakeNonce>(&encoded).unwrap(),
        nonce
    );
}

#[test]
fn frame_credentials_authenticate_the_same_process_sender() {
    let (mut sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    sender
        .send(
            MessageType(5),
            FrameFlags::default(),
            INSTANCE,
            GenerationId(1),
            b"credentials",
            &[],
        )
        .unwrap();
    let identity = receiver.receive().unwrap().peer_identity();
    assert_eq!(identity.pid(), i32::try_from(std::process::id()).unwrap());
    assert_eq!(identity.uid(), rustix::process::getuid().as_raw());
    assert_eq!(identity.gid(), rustix::process::getgid().as_raw());
}

#[test]
fn frame_credentials_authenticate_a_spawned_child_sender() {
    let (sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_credential-sender-fixture"))
        .stdin(Stdio::from(sender.into_owned_fd()))
        .spawn()
        .unwrap();

    let frame = receiver.receive().unwrap();
    assert_eq!(frame.header().sequence().get(), 1);
    assert_eq!(
        frame.peer_identity().pid(),
        i32::try_from(child.id()).unwrap()
    );
    assert!(child.wait().unwrap().success());
}

#[test]
fn malformed_frame_closes_received_descriptor_even_after_sender_copy_is_closed() {
    let (sender, mut receiver) = SeqpacketEndpoint::pair().unwrap();
    let (reader, writer) = UnixStream::pair().unwrap();
    send_raw(&sender, b"bad", &[writer.as_fd()]);
    drop(writer);
    assert!(matches!(
        receiver.receive(),
        Err(TransportError::HeaderTruncated { .. })
    ));
    assert_stream_eof(&reader);
}
