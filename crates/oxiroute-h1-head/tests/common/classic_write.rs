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
