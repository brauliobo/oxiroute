#![no_main]

mod support;

use libfuzzer_sys::fuzz_target;
use pingora_core::protocols::{
    Stream,
    http::v1::{client::HttpSession as ClientSession, server::HttpSession as ServerSession},
};
use tokio::io::AsyncReadExt;
use tokio_test::io::Builder;

const MAX_INPUT_BYTES: usize = 131_072;
const MAX_FRAGMENTS: usize = 256;
const MAX_REQUESTS: usize = 8;
const MAX_RESPONSES: usize = 8;
const MAX_BODY_READS: usize = 32;

#[derive(Clone, Copy)]
enum WireDirection {
    Request,
    Response,
}

fuzz_target!(|data: &[u8]| {
    let Some(data) = support::bounded_input(data, MAX_INPUT_BYTES) else {
        return;
    };
    let data = data.as_ref();
    let (direction, input) = if data == b"seed:oversized-header" {
        (WireDirection::Request, oversized_header())
    } else {
        let (direction, input) = select_direction(data);
        (direction, input.to_vec())
    };
    if input.is_empty() {
        return;
    }

    let stream = fragmented_stream(&input);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    runtime.block_on(async move {
        match direction {
            WireDirection::Request => fuzz_requests(stream).await,
            WireDirection::Response => fuzz_responses(stream).await,
        }
    });
});

fn select_direction(data: &[u8]) -> (WireDirection, &[u8]) {
    for (prefix, direction) in [
        (b"request:".as_slice(), WireDirection::Request),
        (b"response:".as_slice(), WireDirection::Response),
    ] {
        if let Some(input) = support::strip_prefix(data, prefix) {
            return (direction, input);
        }
    }
    if data.starts_with(b"HTTP/") {
        (WireDirection::Response, data)
    } else if data.starts_with(b"GET ")
        || data.starts_with(b"POST ")
        || data.starts_with(b"CONNECT ")
        || data.starts_with(b"HEAD ")
    {
        (WireDirection::Request, data)
    } else {
        let direction = if data.first().copied().unwrap_or_default() & 1 == 0 {
            WireDirection::Request
        } else {
            WireDirection::Response
        };
        (direction, data.get(1..).unwrap_or_default())
    }
}

fn fragmented_stream(input: &[u8]) -> Stream {
    let mut builder = Builder::new();
    let varied = usize::from(input.first().copied().unwrap_or_default() % 31) + 1;
    let minimum = input.len().div_ceil(MAX_FRAGMENTS);
    let fragment_size = varied.max(minimum);
    for fragment in input.chunks(fragment_size).take(MAX_FRAGMENTS) {
        builder.read(fragment);
    }
    Box::new(builder.build())
}

async fn fuzz_requests(stream: Stream) {
    let mut session = ServerSession::new(stream);
    for _ in 0..MAX_REQUESTS {
        let Ok(Some(_)) = session.read_request().await else {
            break;
        };
        let _ = session.req_header();
        let _ = session.get_headers_raw_bytes();
        let _ = session.request_summary();
        for _ in 0..MAX_BODY_READS {
            match session.read_body_bytes().await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        if !session.is_body_done() {
            break;
        }
    }
    let mut stream = session.into_inner();
    let mut remaining = Vec::new();
    let _ = stream.read_to_end(&mut remaining).await;
}

async fn fuzz_responses(stream: Stream) {
    let mut session = ClientSession::new(stream);
    for _ in 0..MAX_RESPONSES {
        let Ok(_) = session.read_response().await else {
            break;
        };
        let _ = session.resp_header();
        let _ = session.get_headers_raw_bytes();
        let informational = session
            .get_status()
            .is_some_and(|status| status.is_informational());
        if informational {
            continue;
        }
        for _ in 0..MAX_BODY_READS {
            match session.read_body_bytes().await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let _ = session.is_body_done();
        break;
    }
    let mut stream = session.into_inner();
    let mut remaining = Vec::new();
    let _ = stream.read_to_end(&mut remaining).await;
}

fn oversized_header() -> Vec<u8> {
    let mut input = b"GET / HTTP/1.1\r\nX-Fuzz: ".to_vec();
    input.extend(std::iter::repeat_n(b'a', MAX_INPUT_BYTES - input.len()));
    input
}
