use std::panic::AssertUnwindSafe;

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};

#[test]
fn rejects_tampered_complex_handshake_peer_packet() {
    let mut client = Handshake::new(PeerType::Client);
    let mut server = Handshake::new(PeerType::Server);
    let client_hello = client.generate_outbound_p0_and_p1().expect("client hello");
    let server_response = match server
        .process_bytes(&client_hello)
        .expect("server response")
    {
        HandshakeProcessResult::InProgress { response_bytes } => response_bytes,
        HandshakeProcessResult::Completed { .. } => panic!("server completed too early"),
    };
    let client_finish = match client
        .process_bytes(&server_response)
        .expect("client finish")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        HandshakeProcessResult::InProgress { .. } => panic!("client did not complete"),
    };

    let mut tampered = client_finish;
    tampered[0] ^= 0x01;

    let error = server
        .process_bytes(&tampered)
        .expect_err("tampered peer packet");
    assert!(matches!(
        error,
        rml_rtmp::handshake::HandshakeError::InvalidP2Packet
    ));
}

#[test]
fn accepts_simple_p2_echo_after_a_complex_client_packet() {
    let mut client = Handshake::new(PeerType::Client);
    let mut server = Handshake::new(PeerType::Server);
    let client_hello = client.generate_outbound_p0_and_p1().expect("client hello");
    let server_response = match server
        .process_bytes(&client_hello)
        .expect("server response")
    {
        HandshakeProcessResult::InProgress { response_bytes } => response_bytes,
        HandshakeProcessResult::Completed { .. } => panic!("server completed too early"),
    };

    let exact_server_p1 = &server_response[1..=1_536];
    let mut simple_p2 = exact_server_p1.to_vec();
    simple_p2.extend_from_slice(&[0xa5, 0x5a]);
    match server.process_bytes(&simple_p2).expect("simple P2 echo") {
        HandshakeProcessResult::Completed {
            response_bytes,
            remaining_bytes,
        } => {
            assert!(response_bytes.is_empty());
            assert_eq!(remaining_bytes, [0xa5, 0x5a]);
        }
        HandshakeProcessResult::InProgress { .. } => {
            panic!("server did not complete the mixed handshake")
        }
    }
}

#[test]
fn malformed_handshake_inputs_do_not_panic() {
    for seed in 0_u32..256 {
        let mut input = Vec::with_capacity((seed as usize % 257) + 1);
        let mut state = seed.wrapping_mul(0x9e37_79b9).wrapping_add(1);
        for _ in 0..input.capacity() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            input.push(state.to_le_bytes()[0]);
        }

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut handshake = Handshake::new(PeerType::Server);
            let _ = handshake.process_bytes(&input);
        }));
        assert!(result.is_ok(), "handshake input {seed} panicked");
    }
}
