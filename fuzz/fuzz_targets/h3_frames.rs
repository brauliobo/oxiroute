#![no_main]

mod support;

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use oxiroute_h3::frame::settings;
use oxiroute_h3::parse::{Control, Event, Fault, RequestParser};

const MAX_INPUT_BYTES: usize = 65_536;
const MAX_HEADERS_FRAME: u64 = 256;

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let piece = 1 + usize::from(mode >> 2) % 61;
    match mode & 3 {
        0 | 1 => request_stream(rest, piece),
        2 => control_stream(rest, piece),
        _ => {
            let _ = settings::parse(rest);
        }
    }
});

/// What a request stream parsed to, however its bytes were split.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    blocks: Vec<Bytes>,
    body: Vec<u8>,
    end: Result<(), Fault>,
}

fn parse_request(bytes: &[u8], piece: usize) -> Outcome {
    let mut parser = RequestParser::new();
    let mut outcome = Outcome {
        blocks: Vec::new(),
        body: Vec::new(),
        end: Ok(()),
    };
    for chunk in bytes.chunks(piece) {
        parser.push(Bytes::copy_from_slice(chunk));
        loop {
            match parser.step(MAX_HEADERS_FRAME) {
                Ok(Some(Event::Headers(block))) => outcome.blocks.push(block),
                Ok(Some(Event::Data(data))) => outcome.body.extend_from_slice(&data),
                Ok(Some(Event::End)) | Ok(None) => break,
                Err(fault) => {
                    outcome.end = Err(fault);
                    return outcome;
                }
            }
        }
    }
    parser.finish_input();
    outcome.end = parser.end().map(|_| ());
    outcome
}

/// The result does not depend on how the bytes were cut into chunks, and no frame payload of more than the bound
/// is ever held back.
fn request_stream(bytes: &[u8], piece: usize) {
    let whole = parse_request(bytes, bytes.len().max(1));
    let split = parse_request(bytes, piece);
    assert_eq!(whole, split);
    let byte_wise = parse_request(bytes, 1);
    assert_eq!(whole, byte_wise);
    assert!(
        whole
            .blocks
            .iter()
            .all(|block| block.len() as u64 <= MAX_HEADERS_FRAME)
    );
}

fn control_stream(bytes: &[u8], piece: usize) {
    let run = |piece: usize| {
        let mut control = Control::new();
        let mut goaways = 0;
        for chunk in bytes.chunks(piece) {
            match control.feed(Bytes::copy_from_slice(chunk)) {
                Ok(goaway) => goaways += usize::from(goaway),
                Err(violation) => return (goaways, Err(violation.0)),
            }
        }
        (goaways, Ok(control.settings()))
    };
    assert_eq!(run(bytes.len().max(1)).1, run(piece).1);
    assert_eq!(run(piece).1, run(1).1);
}
