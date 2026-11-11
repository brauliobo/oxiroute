//! An upgraded connection lives as long as the tunnel does, so the request head it keeps must be its own small
//! allocation and not a view into the read buffer, which is kilobytes of which the head uses a few hundred bytes.

use super::*;
use tokio_test::io::Builder;

const UPGRADE: &[u8] =
    b"GET /chat HTTP/1.1\r\nHost: example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
    Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
const PLAIN: &[u8] =
    b"GET / HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nUser-Agent: bench\r\n\r\n";

async fn read(request: &[u8]) -> HttpSession {
    let mut session = HttpSession::new(Box::new(Builder::new().read(request).build()));
    session.read_request().await.unwrap();
    session
}

fn inside(buffer: &Bytes, value: &http::HeaderValue) -> bool {
    let range = buffer.as_ptr() as usize..buffer.as_ptr() as usize + buffer.len();
    range.contains(&(value.as_bytes().as_ptr() as usize))
}

#[tokio::test]
async fn an_upgrade_request_keeps_no_view_into_the_read_buffer() {
    let session = read(UPGRADE).await;
    assert!(session.is_upgrade_req());
    for (name, value) in session.req_header().headers.iter() {
        assert!(
            !inside(&session.buf, value),
            "{name} still points into the read buffer"
        );
    }
    assert_eq!(session.get_headers_raw_bytes(), UPGRADE);
    assert_eq!(
        session.req_header().headers["sec-websocket-key"],
        "dGhlIHNhbXBsZSBub25jZQ=="
    );
}

#[tokio::test]
async fn a_plain_request_still_serves_its_header_values_from_the_read_buffer() {
    let session = read(PLAIN).await;
    assert!(!session.is_upgrade_req());
    assert!(session
        .req_header()
        .headers
        .values()
        .all(|value| inside(&session.buf, value)));
}
