//! Header block to [`RequestView`]: decodes with the HPACK decoder while copying each field once into the view and
//! applying the RFC 9113 section 8 request validation (pseudo-headers, connection-specific fields,
//! content-length). The one decode path serves both `accept` and `accept_view`.
use http::header::{HeaderName, HeaderValue};
use http::uri::Authority;
use http::{HeaderMap, Method};

use super::validate::{self, UNDECLARED};
use super::view::{MAX_FIELDS, Pseudo, RequestView};
use crate::hpack::{DecodeError, Decoder};

/// What a decoded header block turned out to be.
#[allow(clippy::large_enum_variant)]
pub enum Block {
    Request(RequestView),
    Trailers(HeaderMap),
    /// Violates section 8 of the RFC: a stream error `PROTOCOL_ERROR`.
    Malformed,
    /// The decoded list exceeds `SETTINGS_MAX_HEADER_LIST_SIZE`.
    TooLarge,
}

struct Collector<'a> {
    view: RequestView,
    /// The last `:authority` that passed `http`'s validation: clients repeat it on every request.
    authority: &'a mut Vec<u8>,
    regular: bool,
    bad: bool,
    content_length: Option<u64>,
}

fn method(value: &[u8]) -> bool {
    matches!(value, b"GET" | b"POST") || Method::from_bytes(value).is_ok()
}

impl Collector<'_> {
    fn field(&mut self, name: &[u8], value: &[u8]) {
        if self.bad {
            return;
        }
        if name.first() == Some(&b':') {
            self.bad = self.regular || !self.pseudo(name, value);
            return;
        }
        self.regular = true;
        if self.view.len() < MAX_FIELDS && validate::field(name, value, &mut self.content_length) {
            self.view.push(name, value);
        } else {
            self.bad = true;
        }
    }

    fn pseudo(&mut self, name: &[u8], value: &[u8]) -> bool {
        let (which, value) = match name {
            b":method" => (Pseudo::Method, method(value).then_some(value)),
            b":scheme" => (Pseudo::Scheme, validate::scheme(value).then_some(value)),
            b":authority" => (Pseudo::Authority, self.authority(value).then_some(value)),
            b":path" => (Pseudo::Path, validate::path(value)),
            _ => return false,
        };
        match value {
            Some(value) if !self.view.has(which) => {
                self.view.put(which, value);
                true
            }
            _ => false,
        }
    }

    fn authority(&mut self, value: &[u8]) -> bool {
        if !value.is_empty() && self.authority.as_slice() == value {
            return true;
        }
        if Authority::try_from(value).is_err() {
            return false;
        }
        self.authority.clear();
        self.authority.extend_from_slice(value);
        true
    }

    fn request(self, end_stream: bool) -> Block {
        let view = &self.view;
        let shape_ok = if view.method() == b"CONNECT" {
            view.optional(Pseudo::Authority).is_some()
                && view.optional(Pseudo::Scheme).is_none()
                && view.optional(Pseudo::Path).is_none()
        } else {
            view.optional(Pseudo::Scheme).is_some() && view.optional(Pseudo::Path).is_some()
        };
        let content_length = self.content_length.unwrap_or(UNDECLARED);
        let declared_body = end_stream && content_length != UNDECLARED && content_length > 0;
        if self.bad || !view.has(Pseudo::Method) || !shape_ok || declared_body {
            return Block::Malformed;
        }
        let mut view = self.view;
        view.content_length = content_length;
        Block::Request(view)
    }
}

/// Decodes a request header block.
///
/// # Errors
///
/// The block is not valid HPACK (connection error `COMPRESSION_ERROR`).
pub fn decode_request(
    dec: &mut Decoder,
    authority: &mut Vec<u8>,
    block: &[u8],
    max_list: usize,
    end_stream: bool,
) -> Result<Block, DecodeError> {
    let mut c = Collector {
        view: RequestView::new(end_stream),
        authority,
        regular: false,
        bad: false,
        content_length: None,
    };
    let decoded = dec.decode(block, max_list, |n, v| c.field(n, v))?;
    Ok(if decoded.list_too_large {
        Block::TooLarge
    } else {
        c.request(end_stream)
    })
}

/// Decodes a trailer block (regular fields only).
///
/// # Errors
///
/// The block is not valid HPACK.
pub fn decode_trailers(
    dec: &mut Decoder,
    block: &[u8],
    max_list: usize,
) -> Result<Block, DecodeError> {
    let (mut headers, mut content_length, mut bad) = (HeaderMap::new(), None, false);
    let decoded = dec.decode(block, max_list, |name, value| {
        bad = bad
            || name.first() == Some(&b':')
            || !validate::field(name, value, &mut content_length)
            || headers
                .try_append(
                    HeaderName::from_lowercase(name).expect("validated"),
                    HeaderValue::from_bytes(value).expect("validated"),
                )
                .is_err();
    })?;
    Ok(match (decoded.list_too_large, bad) {
        (true, _) => Block::TooLarge,
        (_, true) => Block::Malformed,
        _ => Block::Trailers(headers),
    })
}

/// Decodes and drops a block, keeping the HPACK state in step.
///
/// # Errors
///
/// The block is not valid HPACK.
pub fn skip(dec: &mut Decoder, block: &[u8]) -> Result<(), DecodeError> {
    dec.decode(block, 0, |_, _| {}).map(|_| ())
}
