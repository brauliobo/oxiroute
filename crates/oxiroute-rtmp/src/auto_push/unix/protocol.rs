#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn handshake_client(
    stream: &mut UnixStream,
    shared: &AutoPushShared,
    expected_worker: WorkerId,
) -> Result<(), RtmpAutoPushError> {
    let nonce = *Uuid::new_v4().as_bytes();
    let secret = shared
        .secret
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(RtmpAutoPushError::CredentialsUnavailable)?;
    let body = hello_body(
        &shared.service_id,
        shared.worker_id,
        nonce,
        &secret,
        b"hello",
    )?;
    write_packet(stream, PACKET_HELLO, &body)
        .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    let (kind, body) = read_packet(stream).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    if kind != PACKET_HELLO_ACK {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    verify_ack(
        &body,
        &shared.service_id,
        expected_worker,
        shared.worker_id,
        nonce,
        &secret,
    )
}

pub(super) fn handshake_server(
    stream: &mut UnixStream,
    shared: &AutoPushShared,
) -> Result<WorkerId, RtmpAutoPushError> {
    let (kind, body) = read_packet(stream).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    if kind != PACKET_HELLO {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    let (service_id, worker_id, nonce, proof) = parse_hello(&body)?;
    if service_id != shared.service_id.as_ref() || worker_id == shared.worker_id {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    let secret = shared
        .secret
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(RtmpAutoPushError::CredentialsUnavailable)?;
    let expected = auth_digest(
        &secret,
        b"hello",
        service_id.as_bytes(),
        &worker_id,
        &nonce,
        &[],
    );
    if !constant_time_eq(&expected, &proof) {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    let body = ack_body(
        &shared.service_id,
        shared.worker_id,
        worker_id,
        nonce,
        &secret,
    )?;
    write_packet(stream, PACKET_HELLO_ACK, &body)
        .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    Ok(worker_id)
}

fn hello_body(
    service_id: &str,
    worker_id: WorkerId,
    nonce: [u8; HELLO_NONCE_BYTES],
    secret: &[u8],
    label: &[u8],
) -> Result<Vec<u8>, RtmpAutoPushError> {
    let mut body = Vec::with_capacity(2 + service_id.len() + 16 + 16 + 32);
    put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
    body.extend_from_slice(&worker_id);
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&auth_digest(
        secret,
        label,
        service_id.as_bytes(),
        &worker_id,
        &nonce,
        &[],
    ));
    Ok(body)
}

fn ack_body(
    service_id: &str,
    server_worker: WorkerId,
    client_worker: WorkerId,
    nonce: [u8; HELLO_NONCE_BYTES],
    secret: &[u8],
) -> Result<Vec<u8>, RtmpAutoPushError> {
    let mut body = Vec::with_capacity(2 + service_id.len() + 16 + 16 + 16 + 32);
    put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
    body.extend_from_slice(&server_worker);
    body.extend_from_slice(&client_worker);
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&auth_digest(
        secret,
        b"ack",
        service_id.as_bytes(),
        &server_worker,
        &nonce,
        &client_worker,
    ));
    Ok(body)
}

fn parse_hello(body: &[u8]) -> Result<(String, WorkerId, [u8; 16], [u8; 32]), RtmpAutoPushError> {
    let mut reader = Reader::new(body);
    let service_id = reader.string(MAX_SERVICE_ID_BYTES)?;
    let worker_id = reader.array::<16>()?;
    let nonce = reader.array::<16>()?;
    let proof = reader.array::<32>()?;
    reader.finish()?;
    Ok((service_id, worker_id, nonce, proof))
}

fn verify_ack(
    body: &[u8],
    service_id: &str,
    expected_server: WorkerId,
    client_worker: WorkerId,
    nonce: [u8; 16],
    secret: &[u8],
) -> Result<(), RtmpAutoPushError> {
    let mut reader = Reader::new(body);
    let received_service = reader.string(MAX_SERVICE_ID_BYTES)?;
    let server_worker = reader.array::<16>()?;
    let received_client = reader.array::<16>()?;
    let received_nonce = reader.array::<16>()?;
    let proof = reader.array::<32>()?;
    reader.finish()?;
    let expected = auth_digest(
        secret,
        b"ack",
        service_id.as_bytes(),
        &server_worker,
        &received_nonce,
        &received_client,
    );
    if received_service != service_id
        || server_worker != expected_server
        || received_client != client_worker
        || received_nonce != nonce
        || !constant_time_eq(&expected, &proof)
    {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    Ok(())
}

pub(super) fn encode_media(
    service_id: &str,
    frame: &AutoPushFrame,
) -> Result<Vec<u8>, RtmpAutoPushError> {
    if frame.application.len() > MAX_STREAM_COMPONENT_BYTES
        || frame.stream_name.len() > MAX_STREAM_COMPONENT_BYTES
        || frame.token.session_id.len() > MAX_SESSION_ID_BYTES
        || frame.event.payload_len() > MAX_PACKET_BYTES
    {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    let mut body = Vec::with_capacity(frame.size());
    put_string(&mut body, service_id, MAX_SERVICE_ID_BYTES)?;
    body.extend_from_slice(&frame.token.worker_id);
    put_string(&mut body, &frame.token.session_id, MAX_SESSION_ID_BYTES)?;
    body.extend_from_slice(&frame.token.incarnation.to_be_bytes());
    body.extend_from_slice(&frame.sequence.to_be_bytes());
    put_string(&mut body, &frame.application, MAX_STREAM_COMPONENT_BYTES)?;
    put_string(&mut body, &frame.stream_name, MAX_STREAM_COMPONENT_BYTES)?;
    body.push(event_kind(frame.event.kind()));
    body.extend_from_slice(&frame.event.timestamp_ms().to_be_bytes());
    let payload_length = u32::try_from(frame.event.payload_len())
        .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    body.extend_from_slice(&payload_length.to_be_bytes());
    body.extend_from_slice(frame.event.payload());
    if body.len() > MAX_PACKET_BYTES {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    Ok(body)
}

pub(super) fn decode_media(
    service_id: &str,
    body: &[u8],
) -> Result<AutoPushFrame, RtmpAutoPushError> {
    if body.len() > MAX_PACKET_BYTES {
        return Err(RtmpAutoPushError::TransportUnavailable);
    }
    let mut reader = Reader::new(body);
    let received_service = reader.string(MAX_SERVICE_ID_BYTES)?;
    if received_service != service_id {
        return Err(RtmpAutoPushError::CredentialsUnavailable);
    }
    let worker_id = reader.array::<16>()?;
    let session_id = reader.string(MAX_SESSION_ID_BYTES)?;
    let incarnation = reader.u64()?;
    let sequence = reader.u64()?;
    let application = reader.string(MAX_STREAM_COMPONENT_BYTES)?;
    let stream_name = reader.string(MAX_STREAM_COMPONENT_BYTES)?;
    let kind = reader.u8()?;
    let timestamp = reader.u32()?;
    let payload_length =
        usize::try_from(reader.u32()?).map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
    let payload = reader.bytes(payload_length)?;
    reader.finish()?;
    let event = decode_event(kind, timestamp, payload)?;
    Ok(AutoPushFrame {
        application,
        stream_name,
        token: SourceToken {
            worker_id,
            session_id,
            incarnation,
        },
        sequence,
        event,
    })
}

pub(in super::super) fn decode_event(
    kind: u8,
    timestamp: u32,
    payload: Vec<u8>,
) -> Result<MediaEvent, RtmpAutoPushError> {
    let event = match kind {
        0 => MediaEvent::from_wire(MediaEventKind::Metadata, timestamp, payload, None)
            .map_err(|_| RtmpAutoPushError::TransportUnavailable),
        1..=8 => {
            let event = if kind == 1 || kind == 2 {
                MediaEvent::audio(timestamp, payload)
            } else {
                MediaEvent::video(timestamp, payload)
            }
            .map_err(|_| RtmpAutoPushError::TransportUnavailable)?;
            if event_kind(event.kind()) != kind {
                return Err(RtmpAutoPushError::TransportUnavailable);
            }
            Ok(event)
        }
        _ => Err(RtmpAutoPushError::TransportUnavailable),
    }?;
    Ok(event)
}

const fn event_kind(kind: MediaEventKind) -> u8 {
    match kind {
        MediaEventKind::Metadata => 0,
        MediaEventKind::AacSequenceHeader => 1,
        MediaEventKind::Audio => 2,
        MediaEventKind::AvcSequenceHeader => 3,
        MediaEventKind::HevcSequenceHeader => 4,
        MediaEventKind::Av1SequenceHeader => 5,
        MediaEventKind::VideoKeyframe => 6,
        MediaEventKind::VideoInterframe => 7,
        MediaEventKind::VideoDisposable => 8,
    }
}

pub(super) fn write_packet(stream: &mut UnixStream, kind: u8, body: &[u8]) -> io::Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "packet too large"))?;
    stream.write_all(&PROTOCOL_MAGIC)?;
    stream.write_all(&[PROTOCOL_VERSION, kind])?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

pub(super) enum PacketReadError {
    Timeout,
    Invalid,
    Io,
}

pub(super) fn read_packet(stream: &mut UnixStream) -> Result<(u8, Vec<u8>), PacketReadError> {
    let mut header = [0; 10];
    stream
        .read_exact(&mut header)
        .map_err(|error| packet_read_error(&error))?;
    if header[..4] != PROTOCOL_MAGIC || header[4] != PROTOCOL_VERSION {
        return Err(PacketReadError::Invalid);
    }
    let length = usize::try_from(u32::from_be_bytes(
        header[6..10]
            .try_into()
            .expect("packet length has four bytes"),
    ))
    .map_err(|_| PacketReadError::Invalid)?;
    if length > MAX_PACKET_BYTES {
        return Err(PacketReadError::Invalid);
    }
    let mut body = vec![0; length];
    stream
        .read_exact(&mut body)
        .map_err(|error| packet_read_error(&error))?;
    Ok((header[5], body))
}

fn packet_read_error(error: &io::Error) -> PacketReadError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        PacketReadError::Timeout
    } else {
        PacketReadError::Io
    }
}

fn auth_digest(
    secret: &[u8],
    label: &[u8],
    service_id: &[u8],
    first_worker: &WorkerId,
    nonce: &[u8; 16],
    second_worker: &[u8],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(secret);
    digest.update(label);
    digest.update(service_id);
    digest.update(first_worker);
    digest.update(nonce);
    digest.update(second_worker);
    digest.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}
