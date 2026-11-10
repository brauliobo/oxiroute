//! Header block to `http::request::Parts`: decodes with the HPACK decoder while building the request and applies
//! the RFC 9113 section 8 request validation (pseudo-headers, connection-specific fields, content-length).
use bytes::Bytes;
use http::header::{self, HeaderName, HeaderValue};
use http::request::Parts;
use http::uri::{Authority, PathAndQuery, Scheme, Uri};
use http::{HeaderMap, Method, Request, Version};

use crate::hpack::{DecodeError, Decoder};

/// What a decoded header block turned out to be.
pub enum Block {
    Request {
        parts: Box<Parts>,
        content_length: u64,
    },
    Trailers(HeaderMap),
    /// Violates section 8 of the RFC: a stream error `PROTOCOL_ERROR`.
    Malformed,
    /// The decoded list exceeds `SETTINGS_MAX_HEADER_LIST_SIZE`.
    TooLarge,
}

/// Declared length, `u64::MAX` when absent.
const UNDECLARED: u64 = u64::MAX;

#[derive(Default)]
struct Collector {
    method: Option<Method>,
    scheme: Option<Scheme>,
    authority: Option<Authority>,
    path: Option<PathAndQuery>,
    headers: HeaderMap,
    regular: bool,
    bad: bool,
    content_length: Option<u64>,
}

impl Collector {
    fn field(&mut self, name: &[u8], value: &[u8], trailers: bool) {
        if name.first() == Some(&b':') {
            if self.regular || trailers {
                self.bad = true;
            } else {
                self.pseudo(name, value);
            }
            return;
        }
        self.regular = true;
        let (Ok(name), Ok(value)) = (
            HeaderName::from_lowercase(name),
            HeaderValue::from_bytes(value),
        ) else {
            self.bad = true;
            return;
        };
        let valid = !is_connection_specific(&name)
            && (name != header::TE || value == "trailers")
            && (name != header::CONTENT_LENGTH || self.content_length(&value));
        self.bad |= !valid || self.headers.try_append(name, value).is_err();
    }

    fn content_length(&mut self, value: &HeaderValue) -> bool {
        let Some(n) = parse_u64(value.as_bytes()) else {
            return false;
        };
        *self.content_length.get_or_insert(n) == n
    }

    fn pseudo(&mut self, name: &[u8], value: &[u8]) {
        let ok = match name {
            b":method" => set(&mut self.method, Method::from_bytes(value).ok()),
            b":scheme" => set(&mut self.scheme, Scheme::try_from(value).ok()),
            b":authority" => set(
                &mut self.authority,
                Authority::from_maybe_shared(Bytes::copy_from_slice(value)).ok(),
            ),
            b":path" => {
                !value.is_empty()
                    && set(
                        &mut self.path,
                        PathAndQuery::from_maybe_shared(Bytes::copy_from_slice(value)).ok(),
                    )
            }
            _ => false,
        };
        self.bad |= !ok;
    }

    fn request(self, end_stream: bool) -> Block {
        let connect = self.method.as_ref() == Some(&Method::CONNECT);
        let Some(method) = self.method.filter(|_| !self.bad) else {
            return Block::Malformed;
        };
        let shape_ok = if connect {
            self.authority.is_some() && self.scheme.is_none() && self.path.is_none()
        } else {
            self.scheme.is_some() && self.path.is_some()
        };
        let content_length = self.content_length.unwrap_or(UNDECLARED);
        if !shape_ok || (end_stream && content_length != UNDECLARED && content_length > 0) {
            return Block::Malformed;
        }
        let mut uri = http::uri::Parts::default();
        uri.scheme = self.scheme.filter(|_| self.authority.is_some());
        uri.authority = self.authority;
        uri.path_and_query = self.path;
        let Ok(uri) = Uri::from_parts(uri) else {
            return Block::Malformed;
        };
        let (mut parts, ()) = Request::new(()).into_parts();
        parts.method = method;
        parts.uri = uri;
        parts.version = Version::HTTP_2;
        parts.headers = self.headers;
        Block::Request {
            parts: Box::new(parts),
            content_length,
        }
    }
}

fn set<T>(slot: &mut Option<T>, value: Option<T>) -> bool {
    match (slot.is_none(), value) {
        (true, Some(v)) => {
            *slot = Some(v);
            true
        }
        _ => false,
    }
}

fn is_connection_specific(name: &HeaderName) -> bool {
    name == header::CONNECTION
        || name == header::TRANSFER_ENCODING
        || name == header::UPGRADE
        || name == "keep-alive"
        || name == "proxy-connection"
}

/// Digits only, at most 19 of them (so the value cannot overflow).
fn parse_u64(src: &[u8]) -> Option<u64> {
    if src.is_empty() || src.len() > 19 || !src.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(src.iter().fold(0, |n, d| n * 10 + u64::from(d - b'0')))
}

/// Decodes a request header block.
///
/// # Errors
///
/// The block is not valid HPACK (connection error `COMPRESSION_ERROR`).
pub fn decode_request(
    dec: &mut Decoder,
    block: &[u8],
    max_list: usize,
    end_stream: bool,
) -> Result<Block, DecodeError> {
    let mut c = Collector::default();
    let decoded = dec.decode(block, max_list, |n, v| c.field(n, v, false))?;
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
    let mut c = Collector::default();
    let decoded = dec.decode(block, max_list, |n, v| c.field(n, v, true))?;
    Ok(match (decoded.list_too_large, c.bad) {
        (true, _) => Block::TooLarge,
        (_, true) => Block::Malformed,
        _ => Block::Trailers(c.headers),
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

/// Declared content-length marker for streams without one.
pub const NO_CONTENT_LENGTH: u64 = UNDECLARED;
