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
fn descriptor_manifest_version_one_payload_stays_byte_exact() {
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(7),
        role: DescriptorRole::Management,
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp("127.0.0.1:9900".parse().unwrap())),
        mode: None,
    }])
    .unwrap();

    assert_eq!(
        encode_adopt_request(1, &manifest).unwrap(),
        [
            0, 2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 1, 0, 7, 2, 1, 1, 127, 0, 0, 1, 38, 172,
            0,
        ]
    );
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
