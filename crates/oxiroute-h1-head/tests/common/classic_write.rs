//! The classic request write: `RequestHeader` edits through the real `HeaderMap` calls, then `header_to_h1_wire`
//! behind the request line of `http_req_header_to_wire` (vendored `v1/client.rs`).

use super::classic_request::ClassicRequest;
use http::header::{HeaderName, HeaderValue};
use oxiroute_h1_head::{Edits, Name, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Remove,
    Set,
    Add,
}

/// An edit that owns its bytes, so a test can generate it and lend it to both sides.
#[derive(Clone, Debug)]
pub struct OwnedEdit {
    pub kind: Kind,
    pub name: &'static str,
    pub value: Vec<u8>,
}

pub fn lend(owned: &[OwnedEdit]) -> Edits<'_> {
    let mut edits = Edits::new();
    for e in owned {
        let name = Name::new(e.name.as_bytes()).expect("a lowercase token");
        let value = Value::new(&e.value).expect("a valid value");
        match e.kind {
            Kind::Remove => edits.remove(name).unwrap(),
            Kind::Set => edits.set(name, value).unwrap(),
            Kind::Add => edits.add(name, value).unwrap(),
        }
    }
    edits
}

pub fn apply(map: &mut http::HeaderMap, edits: &[OwnedEdit]) {
    for e in edits {
        let name = HeaderName::from_bytes(e.name.as_bytes()).unwrap();
        match e.kind {
            Kind::Remove => {
                map.remove(name);
            }
            Kind::Set => {
                map.insert(name, HeaderValue::from_bytes(&e.value).unwrap());
            }
            Kind::Add => {
                map.append(name, HeaderValue::from_bytes(&e.value).unwrap());
            }
        }
    }
}

pub fn wire(mut request: ClassicRequest, edits: &[OwnedEdit]) -> Vec<u8> {
    apply(&mut request.header.headers, edits);
    let mut out = Vec::new();
    out.extend_from_slice(request.header.method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(request.header.raw_path());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    request.header.header_to_h1_wire(&mut out);
    out.extend_from_slice(b"\r\n");
    out
}

/// `titled_name` of the vendored `v1/server.rs`.
fn titled_name(name: &HeaderName) -> &str {
    use http::header;
    match *name {
        header::ACCEPT_RANGES => "Accept-Ranges",
        header::AGE => "Age",
        header::CACHE_CONTROL => "Cache-Control",
        header::CONNECTION => "Connection",
        header::CONTENT_TYPE => "Content-Type",
        header::CONTENT_ENCODING => "Content-Encoding",
        header::CONTENT_LENGTH => "Content-Length",
        header::DATE => "Date",
        header::TRANSFER_ENCODING => "Transfer-Encoding",
        header::HOST => "Host",
        header::SERVER => "Server",
        header::SET_COOKIE => "Set-Cookie",
        _ => name.as_str(),
    }
}

/// `http_resp_header_to_buf` of the vendored `v1/server.rs`, with the date text and connection word given.
pub fn serialize_response(
    resp: &pingora_http::ResponseHeader,
    minor: u8,
    automatic: Option<(&str, &str)>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(if minor == 0 {
        b"HTTP/1.0 "
    } else {
        b"HTTP/1.1 "
    });
    buf.extend_from_slice(resp.status.as_str().as_bytes());
    buf.push(b' ');
    if let Some(reason) = resp.get_reason_phrase() {
        buf.extend_from_slice(reason.as_bytes());
    }
    buf.extend_from_slice(b"\r\n");
    match automatic {
        None => resp.header_to_h1_wire(&mut buf),
        Some((date, connection)) => {
            for (name, value) in &resp.headers {
                if name == http::header::DATE || name == http::header::CONNECTION {
                    continue;
                }
                buf.extend_from_slice(titled_name(name).as_bytes());
                buf.extend_from_slice(b": ");
                buf.extend_from_slice(value.as_bytes());
                buf.extend_from_slice(b"\r\n");
            }
            buf.extend_from_slice(
                format!("Date: {date}\r\nConnection: {connection}\r\n").as_bytes(),
            );
        }
    }
    buf.extend_from_slice(b"\r\n");
    buf
}

/// `stamp_response` of `crates/oxiroute-server/src/http/request_id.rs`.
pub fn stamp_response(response: &mut pingora_http::ResponseHeader, id: &HeaderValue) {
    use http::header::Entry;
    let request_id = HeaderName::from_static("x-request-id");
    if let Entry::Vacant(slot) = response.headers.entry(request_id.clone()) {
        slot.insert(id.clone());
        return;
    }
    if let Some(upstream) = response.remove_header(&request_id)
        && upstream != *id
    {
        response
            .insert_header(HeaderName::from_static("x-upstream-request-id"), upstream)
            .unwrap();
    }
    response.insert_header(request_id, id.clone()).unwrap();
}
