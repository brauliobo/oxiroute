//! Hand-written HTTP/3 requests for a raw quinn client.
#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_h3::frame::{self, settings};

pub(super) fn raw_request(method: &Method, path: &str) -> Vec<u8> {
    let method_line = if method == Method::HEAD { 0xd2 } else { 0xd1 };
    let mut block = vec![0, 0, method_line, 0xd7, 0x50, 12];
    block.extend_from_slice(b"example.test");
    block.extend_from_slice(&[0x51, u8::try_from(path.len()).expect("short path")]);
    block.extend_from_slice(path.as_bytes());
    let mut out = Vec::new();
    frame::write_header(&mut out, frame::HEADERS, block.len() as u64);
    out.extend_from_slice(&block);
    out
}

/// Opens the client control stream with `SETTINGS`; the stream must stay alive as long as the connection.
pub(super) async fn open_control(connection: &quinn::Connection) -> quinn::SendStream {
    let mut control = connection.open_uni().await.expect("control stream");
    let mut hello = vec![0x00];
    settings::encode(&mut hello, 16 * 1024);
    control.write_all(&hello).await.expect("settings");
    control
}
