use oxiroute_rtmp::{
    simple_handshake_response, HandshakeError, HANDSHAKE_BLOCK_SIZE, RTMP_VERSION,
};

#[test]
fn responds_to_a_simple_client_hello() {
    let client_block = client_block();
    let mut client_hello = Vec::with_capacity(HANDSHAKE_BLOCK_SIZE + 1);
    client_hello.push(RTMP_VERSION);
    client_hello.extend_from_slice(&client_block);
    let random = [0x5a; HANDSHAKE_BLOCK_SIZE - 8];

    let response = simple_handshake_response(&client_hello, 0x0102_0304, &random)
        .expect("valid simple handshake");

    assert_eq!(response.len(), 1 + HANDSHAKE_BLOCK_SIZE * 2);
    assert_eq!(response[0], RTMP_VERSION);
    assert_eq!(&response[1..5], &0x0102_0304_u32.to_be_bytes());
    assert_eq!(&response[5..9], &[0; 4]);
    assert_eq!(&response[9..=HANDSHAKE_BLOCK_SIZE], &random);
    assert_eq!(&response[1 + HANDSHAKE_BLOCK_SIZE..], &client_block);
}

#[test]
fn rejects_invalid_hello_length_and_version() {
    let random = [0; HANDSHAKE_BLOCK_SIZE - 8];

    assert_eq!(
        simple_handshake_response(&[RTMP_VERSION], 0, &random),
        Err(HandshakeError::InvalidLength(1))
    );

    let mut hello = vec![0; HANDSHAKE_BLOCK_SIZE + 1];
    hello[0] = 2;
    assert_eq!(
        simple_handshake_response(&hello, 0, &random),
        Err(HandshakeError::UnsupportedVersion(2))
    );
}

fn client_block() -> [u8; HANDSHAKE_BLOCK_SIZE] {
    let mut block = [0; HANDSHAKE_BLOCK_SIZE];
    block[..4].copy_from_slice(&0x1122_3344_u32.to_be_bytes());
    for (index, byte) in block[8..].iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).expect("bounded test byte");
    }
    block
}
