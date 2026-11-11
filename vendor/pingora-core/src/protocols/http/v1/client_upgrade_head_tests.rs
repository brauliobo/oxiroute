//! The upstream session of an upgraded connection must not keep the buffer its 101 head was read into (see
//! `server_upgrade_head_tests.rs`); bytes the origin sent after the head still belong to the tunnel.

use super::*;
use tokio_test::io::Builder;

const WIRE: &[u8] =
    b"GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: WS\r\nContent-Length: 0\r\n\r\n";
const SWITCHING: &[u8] =
    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: WS\r\nSec-WebSocket-Accept: abc\r\n\r\nPAYLOAD";

async fn upgraded() -> HttpSession {
    let mut session =
        HttpSession::new(Box::new(Builder::new().write(WIRE).read(SWITCHING).build()));
    let mut request = RequestHeader::build("GET", b"/", None).unwrap();
    request.insert_header("Connection", "Upgrade").unwrap();
    request.insert_header("Upgrade", "WS").unwrap();
    request.insert_header("Content-Length", "0").unwrap();
    session
        .write_request_header(Box::new(request))
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn a_switching_protocols_head_keeps_no_view_into_the_read_buffer() {
    let mut session = upgraded().await;
    session.read_response().await.unwrap();
    assert!(session.was_upgraded());
    let head = session.resp_header().unwrap();
    for (name, value) in head.headers.iter() {
        let range =
            session.buf.as_ptr() as usize..session.buf.as_ptr() as usize + session.buf.len();
        assert!(
            !range.contains(&(value.as_bytes().as_ptr() as usize)),
            "{name} still points into the read buffer"
        );
    }
    assert_eq!(head.headers["sec-websocket-accept"], "abc");
}

#[tokio::test]
async fn bytes_after_a_switching_protocols_head_are_still_delivered() {
    let mut session = upgraded().await;
    session.read_response().await.unwrap();
    session.maybe_upgrade_body_writer();
    match session.read_response_task().await.unwrap() {
        HttpTask::UpgradedBody(body, _) => assert_eq!(body.unwrap(), &b"PAYLOAD"[..]),
        _ => panic!("the bytes after the head are the first body of the tunnel"),
    }
}
