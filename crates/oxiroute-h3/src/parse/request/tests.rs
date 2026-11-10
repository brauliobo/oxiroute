use super::*;

const LIMIT: u64 = 100;

fn frame_bytes(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame::write_header(&mut out, kind, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

/// Runs all of `bytes`, delivered in pieces of `piece` bytes, and collects the events up to the end of the stream.
fn run(bytes: &[u8], piece: usize) -> Result<Vec<Event>, Fault> {
    let mut parser = RequestParser::new();
    let mut events = Vec::new();
    for chunk in bytes.chunks(piece) {
        parser.push(Bytes::copy_from_slice(chunk));
        while let Some(event) = parser.step(LIMIT)? {
            events.push(event);
        }
    }
    parser.finish_input();
    events.push(parser.end()?);
    Ok(events)
}

fn merge(events: Vec<Event>) -> (Vec<Bytes>, Vec<u8>) {
    let (mut headers, mut data) = (Vec::new(), Vec::new());
    for event in events {
        match event {
            Event::Headers(block) => headers.push(block),
            Event::Data(piece) => data.extend_from_slice(&piece),
            Event::End => {}
        }
    }
    (headers, data)
}

fn message() -> Vec<u8> {
    let mut bytes = frame_bytes(frame::HEADERS, &[1, 2, 3, 4, 5]);
    bytes.extend(frame_bytes(0x1f * 3 + 0x21, &[9; 40]));
    bytes.extend(frame_bytes(frame::DATA, b"hello "));
    bytes.extend(frame_bytes(frame::DATA, b"world"));
    bytes.extend(frame_bytes(frame::HEADERS, &[7, 7]));
    bytes
}

#[test]
fn a_message_parses_the_same_at_every_split() {
    for piece in [1, 2, 3, 5, 7, 64, 1000] {
        let (headers, data) = merge(run(&message(), piece).unwrap());
        assert_eq!(
            headers,
            [
                Bytes::from_static(&[1, 2, 3, 4, 5]),
                Bytes::from_static(&[7, 7])
            ],
            "piece {piece}"
        );
        assert_eq!(data, b"hello world", "piece {piece}");
    }
}

#[test]
fn an_empty_headers_frame_is_an_event_and_an_empty_data_frame_is_not() {
    let mut bytes = frame_bytes(frame::HEADERS, &[]);
    bytes.extend(frame_bytes(frame::DATA, &[]));
    let events = run(&bytes, 1).unwrap();
    assert_eq!(events, [Event::Headers(Bytes::new()), Event::End]);
}

#[test]
fn data_before_the_head_and_frames_after_the_trailers_are_unexpected() {
    let unexpected = |fault: Fault| matches!(fault, Fault::Connection(code, _) if code == Code::H3_FRAME_UNEXPECTED);
    assert!(unexpected(
        run(&frame_bytes(frame::DATA, b"x"), 3).unwrap_err()
    ));
    let mut bytes = message();
    bytes.extend(frame_bytes(frame::HEADERS, &[]));
    assert!(unexpected(run(&bytes, 3).unwrap_err()));
    let mut bytes = frame_bytes(frame::HEADERS, &[]);
    bytes.extend(frame_bytes(frame::HEADERS, &[]));
    bytes.extend(frame_bytes(frame::DATA, &[1]));
    assert!(unexpected(run(&bytes, 3).unwrap_err()));
    for kind in [
        frame::SETTINGS,
        frame::GOAWAY,
        frame::PUSH_PROMISE,
        frame::CANCEL_PUSH,
        frame::MAX_PUSH_ID,
        2,
        6,
        8,
        9,
    ] {
        assert!(
            unexpected(run(&frame_bytes(kind, &[0]), 3).unwrap_err()),
            "{kind:#x}"
        );
    }
}

#[test]
fn headers_over_the_bound_is_a_stream_fault_before_any_payload_is_read() {
    let mut header = Vec::new();
    frame::write_header(&mut header, frame::HEADERS, LIMIT + 1);
    let mut parser = RequestParser::new();
    parser.push(Bytes::from(header));
    assert!(
        matches!(parser.step(LIMIT), Err(Fault::Stream(code, _)) if code == Code::H3_EXCESSIVE_LOAD)
    );
    let (headers, _) = merge(run(&frame_bytes(frame::HEADERS, &[0; LIMIT as usize]), 9).unwrap());
    assert_eq!(headers[0].len(), LIMIT as usize);
}

#[test]
fn ending_inside_a_frame_is_a_frame_error() {
    for cut in [1, 2, 4, 9] {
        let bytes = &message()[..cut];
        let error = run(bytes, 100).unwrap_err();
        assert!(
            matches!(error, Fault::Connection(code, _) if code == Code::H3_FRAME_ERROR),
            "cut {cut}"
        );
    }
}

#[test]
fn skipped_frames_hold_no_memory() {
    let mut parser = RequestParser::new();
    let mut header = Vec::new();
    frame::write_header(&mut header, 0x1f * 3 + 0x21, 1 << 40);
    parser.push(Bytes::from(header));
    assert_eq!(parser.step(LIMIT), Ok(None));
    for _ in 0..100 {
        parser.push(Bytes::from(vec![0u8; 65_536]));
        assert_eq!(parser.step(LIMIT), Ok(None));
    }
    assert!(parser.block.capacity() == 0 && !parser.has_input());
}
