// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Common functions and constants

use http::{header, HeaderName, HeaderValue};
use log::warn;
use pingora_error::{Error, ErrorType::*, Result};
use pingora_http::{HMap, RequestHeader, ResponseHeader};
use std::str;
use std::time::Duration;

use super::body::BodyWriter;
use crate::utils::KVRef;

pub(super) const MAX_HEADERS: usize = 256;

/// Room for the headers of one message. The parser initialises the slots it fills, so nothing is written here
/// (`[EMPTY_HEADER; MAX_HEADERS]` stored 8 KiB on every parse attempt).
pub(super) type HeaderSlots<'buf> = [std::mem::MaybeUninit<httparse::Header<'buf>>; MAX_HEADERS];

pub(super) fn header_slots<'buf>() -> HeaderSlots<'buf> {
    [const { std::mem::MaybeUninit::uninit() }; MAX_HEADERS]
}

pub(super) const INIT_HEADER_BUF_SIZE: usize = 4096;
pub(super) const MAX_HEADER_SIZE: usize = 1048575;

pub(crate) const BODY_BUF_LIMIT: usize = 1024 * 64;

pub const CRLF: &[u8; 2] = b"\r\n";
pub const HEADER_KV_DELIMITER: &[u8; 2] = b": ";

pub(super) enum HeaderParseState {
    Complete(usize),
    Partial,
    Invalid(httparse::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum KeepaliveStatus {
    Timeout(Duration),
    Infinite,
    Off,
}

struct ConnectionValue {
    keep_alive: bool,
    upgrade: bool,
    close: bool,
}

impl ConnectionValue {
    fn new() -> Self {
        ConnectionValue {
            keep_alive: false,
            upgrade: false,
            close: false,
        }
    }

    fn close(mut self) -> Self {
        self.close = true;
        self
    }
    fn upgrade(mut self) -> Self {
        self.upgrade = true;
        self
    }
    fn keep_alive(mut self) -> Self {
        self.keep_alive = true;
        self
    }
}

fn parse_connection_header(value: &[u8]) -> ConnectionValue {
    // only parse keep-alive, close, and upgrade tokens
    // https://www.rfc-editor.org/rfc/rfc9110.html#section-7.6.1

    const KEEP_ALIVE: &str = "keep-alive";
    const CLOSE: &str = "close";
    const UPGRADE: &str = "upgrade";

    // fast path
    if value.eq_ignore_ascii_case(CLOSE.as_bytes()) {
        ConnectionValue::new().close()
    } else if value.eq_ignore_ascii_case(KEEP_ALIVE.as_bytes()) {
        ConnectionValue::new().keep_alive()
    } else if value.eq_ignore_ascii_case(UPGRADE.as_bytes()) {
        ConnectionValue::new().upgrade()
    } else {
        // slow path, parse the connection value
        let mut close = false;
        let mut upgrade = false;
        let value = str::from_utf8(value).unwrap_or("");
        for token in value
            .split(',')
            .map(|s| s.trim())
            .filter(|&x| !x.is_empty())
        {
            if token.eq_ignore_ascii_case(CLOSE) {
                close = true;
            } else if token.eq_ignore_ascii_case(UPGRADE) {
                upgrade = true;
            }
            if upgrade && close {
                return ConnectionValue::new().upgrade().close();
            }
        }
        if close {
            ConnectionValue::new().close()
        } else if upgrade {
            ConnectionValue::new().upgrade()
        } else {
            ConnectionValue::new()
        }
    }
}

pub(crate) fn init_body_writer_comm(body_writer: &mut BodyWriter, headers: &HMap) {
    if is_chunked_encoding_from_headers(headers) {
        // transfer-encoding takes priority over content-length
        body_writer.init_chunked();
    } else {
        let content_length = header_value_content_length(headers.get(http::header::CONTENT_LENGTH));
        match content_length {
            Some(length) => {
                body_writer.init_content_length(length);
            }
            None => {
                /* TODO: 1. connection: keepalive cannot be used,
                2. mark connection must be closed */
                body_writer.init_close_delimited();
            }
        }
    }
}

/// Find the last comma-separated token in a Transfer-Encoding header value.
/// Takes the literal last token after the last comma, even if empty.
#[inline]
fn find_last_te_token(bytes: &[u8]) -> &[u8] {
    let last_token = bytes
        .iter()
        .rposition(|&b| b == b',')
        .map(|pos| &bytes[pos + 1..])
        .unwrap_or(bytes);

    last_token.trim_ascii()
}

/// Check if chunked encoding is the final encoding across all transfer-encoding headers
pub(crate) fn is_chunked_encoding_from_headers(headers: &HMap) -> bool {
    // Get the last Transfer-Encoding header value
    let last_te = headers
        .get_all(http::header::TRANSFER_ENCODING)
        .into_iter()
        .next_back();

    let Some(last_header_value) = last_te else {
        return false;
    };

    let bytes = last_header_value.as_bytes();

    // Fast path: exact match for "chunked"
    if bytes.eq_ignore_ascii_case(b"chunked") {
        return true;
    }

    // Slow path: parse comma-separated values
    find_last_te_token(bytes).eq_ignore_ascii_case(b"chunked")
}

pub fn is_upgrade_req(req: &RequestHeader) -> bool {
    req.version == http::Version::HTTP_11 && req.headers.get(header::UPGRADE).is_some()
}

pub fn is_expect_continue_req(req: &RequestHeader) -> bool {
    req.version == http::Version::HTTP_11
        // https://www.rfc-editor.org/rfc/rfc9110#section-10.1.1
        && req.headers.get(header::EXPECT).is_some_and(|v| {
            v.as_bytes().eq_ignore_ascii_case(b"100-continue")
        })
}

// Unlike the upgrade check on request, this function doesn't check the Upgrade or Connection header
// because when seeing 101, we assume the server accepts to switch protocol.
// In reality it is not common that some servers don't send all the required headers to establish
// websocket connections.
pub fn is_upgrade_resp(header: &ResponseHeader) -> bool {
    header.status == 101 && header.version == http::Version::HTTP_11
}

#[inline]
pub fn header_value_content_length(
    header_value: Option<&http::header::HeaderValue>,
) -> Option<usize> {
    match header_value {
        Some(value) => buf_to_content_length(Some(value.as_bytes())).ok().flatten(),
        None => None,
    }
}

#[inline]
pub(super) fn buf_to_content_length(header_value: Option<&[u8]>) -> Result<Option<usize>> {
    match header_value {
        Some(buf) => {
            match str::from_utf8(buf) {
                // check valid string
                Ok(str_cl_value) => match str_cl_value.parse::<i64>() {
                    Ok(cl_length) => {
                        if cl_length >= 0 {
                            Ok(Some(cl_length as usize))
                        } else {
                            warn!("negative content-length header value {cl_length}");
                            Error::e_explain(
                                InvalidHTTPHeader,
                                format!("negative Content-Length header value: {cl_length}"),
                            )
                        }
                    }
                    Err(_) => {
                        warn!("invalid content-length header value {str_cl_value}");
                        Error::e_explain(
                            InvalidHTTPHeader,
                            format!("invalid Content-Length header value: {str_cl_value}"),
                        )
                    }
                },
                Err(_) => {
                    warn!("invalid content-length header encoding");
                    Error::e_explain(InvalidHTTPHeader, "invalid Content-Length header encoding")
                }
            }
        }
        None => Ok(None),
    }
}

#[inline]
pub(super) fn is_buf_keepalive(header_value: Option<&HeaderValue>) -> Option<bool> {
    header_value.and_then(|value| {
        let value = parse_connection_header(value.as_bytes());
        if value.keep_alive {
            Some(true)
        } else if value.close {
            Some(false)
        } else {
            None
        }
    })
}

/// The offsets of one message's headers; up to this many need no allocation.
pub(super) type HeaderRefs = smallvec::SmallVec<[KVRef; 16]>;

#[inline]
pub(super) fn populate_headers(
    base: usize,
    header_ref: &mut HeaderRefs,
    headers: &[httparse::Header],
) -> usize {
    let mut used_header_index = 0;
    for header in headers.iter() {
        if !header.name.is_empty() {
            header_ref.push(KVRef::new(
                header.name.as_ptr() as usize - base,
                header.name.len(),
                header.value.as_ptr() as usize - base,
                header.value.len(),
            ));
            used_header_index += 1;
        }
    }
    used_header_index
}

/// Names seen per thread. A header name outside the standard table is allocated every time it is parsed; real
/// traffic repeats the same few dozen names, so remembering them turns that allocation into a hash lookup.
const NAME_CACHE_ENTRIES: usize = 512;
const NAME_CACHE_MAX_BYTES: usize = 64;

thread_local! {
    static NAMES: std::cell::RefCell<NameCache> = std::cell::RefCell::new(NameCache::default());
}

type NameCache =
    std::collections::HashMap<Box<[u8]>, HeaderName, std::hash::BuildHasherDefault<FnvHasher>>;

/// FNV-1a: header names are short and the cache is bounded, so the default keyed hash buys nothing.
struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

fn header_name(name: &[u8]) -> Result<HeaderName> {
    if name.len() > NAME_CACHE_MAX_BYTES {
        return HeaderName::from_bytes(name).map_err(|_| Error::new(InvalidHTTPHeader));
    }
    NAMES.with_borrow_mut(|names| {
        if let Some(cached) = names.get(name) {
            return Ok(cached.clone());
        }
        let parsed = HeaderName::from_bytes(name).map_err(|_| Error::new(InvalidHTTPHeader))?;
        if names.len() >= NAME_CACHE_ENTRIES {
            names.clear();
        }
        names.insert(name.into(), parsed.clone());
        Ok(parsed)
    })
}

/// What a request head says about its framing and connection handling, noted while its headers are appended so
/// that the checks that follow read a flag instead of hashing a header name each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestFacts {
    pub transfer_encoding: bool,
    /// The number of `Content-Length` headers, 2 standing for any more than one.
    pub content_lengths: u8,
    pub connection: bool,
    pub upgrade: bool,
}

impl RequestFacts {
    /// Nothing is known: every header may be present, so a reader looks it up.
    pub const UNKNOWN: Self = Self {
        transfer_encoding: true,
        content_lengths: 1,
        connection: true,
        upgrade: true,
    };

    /// The facts of headers that were not noted as they were appended.
    pub fn of(headers: &HMap) -> Self {
        Self {
            transfer_encoding: headers.contains_key(header::TRANSFER_ENCODING),
            content_lengths: headers
                .get_all(header::CONTENT_LENGTH)
                .iter()
                .take(2)
                .count() as u8,
            connection: headers.contains_key(header::CONNECTION),
            upgrade: headers.contains_key(header::UPGRADE),
        }
    }

    fn note(&mut self, name: &HeaderName) {
        if *name == header::CONTENT_LENGTH {
            self.content_lengths = (self.content_lengths + 1).min(2);
        } else if *name == header::TRANSFER_ENCODING {
            self.transfer_encoding = true;
        } else if *name == header::CONNECTION {
            self.connection = true;
        } else if *name == header::UPGRADE {
            self.upgrade = true;
        }
    }
}

/// Whether `bytes` are valid `HeaderValue` contents: the check `HeaderValue::from_bytes` makes, without copying them.
/// Eight bytes at a time are tested for a control byte or DEL; a tab (valid) or a real offender sends the value to the
/// byte-wise check.
#[inline]
pub(super) fn is_header_value(bytes: &[u8]) -> bool {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let valid = |&byte: &u8| (byte >= 32 && byte != 127) || byte == b'\t';
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(<[u8; 8]>::try_from(chunk).expect("a chunk of eight"));
        let below_space = word.wrapping_sub(ONES * 0x20) & !word & HIGH;
        let deleted = word ^ (ONES * 0x7f);
        let has_del = deleted.wrapping_sub(ONES) & !deleted & HIGH;
        if below_space | has_del != 0 {
            return bytes.iter().all(valid);
        }
    }
    chunks.remainder().iter().all(valid)
}

/// Appends a header validated by the HTTP/1 parser without retaining its wire casing, and notes in `facts`
/// what the header is.
#[inline]
pub(super) fn append_parsed_request_header(
    headers: &mut HMap,
    name: &[u8],
    value: HeaderValue,
    facts: &mut RequestFacts,
) -> Result<()> {
    let name = header_name(name)?;
    facts.note(&name);
    headers.append(name, value);
    Ok(())
}

// RFC 7230:
// If a message is received without Transfer-Encoding and with
// either multiple Content-Length header fields having differing
// field-values or a single Content-Length header field having an
// invalid value, then the message framing is invalid and the
// recipient MUST treat it as an unrecoverable error.
#[cfg(test)]
pub(super) fn check_dup_content_length(headers: &HMap) -> Result<()> {
    if headers.get(header::TRANSFER_ENCODING).is_some() {
        // If TE header, ignore CL
        return Ok(());
    }
    let mut cls = headers.get_all(header::CONTENT_LENGTH).into_iter();
    if cls.next().is_none() {
        // no CL header is fine.
        return Ok(());
    }
    if cls.next().is_some() {
        // duplicated CL is bad
        return crate::Error::e_explain(
            crate::ErrorType::InvalidHTTPHeader,
            "duplicated Content-Length header",
        );
    }
    Ok(())
}

/// Refuses duplicate `Content-Length` headers, from the facts noted while the message was parsed.
pub(super) fn check_dup_content_length_facts(facts: RequestFacts) -> Result<()> {
    // If a Transfer-Encoding is present, Content-Length is ignored.
    if !facts.transfer_encoding && facts.content_lengths > 1 {
        return crate::Error::e_explain(
            crate::ErrorType::InvalidHTTPHeader,
            "duplicated Content-Length header",
        );
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use http::{
        header::{CONTENT_LENGTH, TRANSFER_ENCODING},
        StatusCode, Version,
    };
    use rstest::rstest;

    #[test]
    fn cached_header_names_equal_parsed_ones() {
        for _ in 0..2 {
            assert_eq!(
                header_name(b"X-Custom-Name").unwrap(),
                HeaderName::from_static("x-custom-name")
            );
        }
        assert!(header_name(b"bad name").is_err());
        for index in 0..NAME_CACHE_ENTRIES * 2 {
            header_name(format!("x-n-{index}").as_bytes()).unwrap();
        }
        assert_eq!(header_name(b"Host").unwrap(), header::HOST);
        assert!(header_name(&[b'a'; NAME_CACHE_MAX_BYTES + 1]).is_ok());
    }

    #[test]
    fn header_value_validity_matches_the_http_crate() {
        for byte in 0..=u8::MAX {
            assert_eq!(
                is_header_value(&[b'a', byte]),
                HeaderValue::from_bytes(&[b'a', byte]).is_ok(),
                "byte {byte}"
            );
        }
        assert!(is_header_value(b""));
        // Every byte value at every position of values longer than a word, so the word-wise path is checked.
        for length in [7, 8, 9, 15, 16, 17, 40] {
            for position in 0..length {
                for byte in 0..=u8::MAX {
                    let mut value = vec![b'a'; length];
                    value[position] = byte;
                    assert_eq!(
                        is_header_value(&value),
                        HeaderValue::from_bytes(&value).is_ok(),
                        "byte {byte} at {position} of {length}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_check_dup_content_length() {
        let mut headers = HMap::new();

        assert!(check_dup_content_length(&headers).is_ok());

        headers.append(CONTENT_LENGTH, "1".try_into().unwrap());
        assert!(check_dup_content_length(&headers).is_ok());

        headers.append(CONTENT_LENGTH, "2".try_into().unwrap());
        assert!(check_dup_content_length(&headers).is_err());

        headers.append(TRANSFER_ENCODING, "chunkeds".try_into().unwrap());
        assert!(check_dup_content_length(&headers).is_ok());
    }

    #[test]
    fn test_is_upgrade_resp() {
        let mut response = ResponseHeader::build(StatusCode::SWITCHING_PROTOCOLS, None).unwrap();
        response.set_version(Version::HTTP_11);
        response.insert_header("Upgrade", "websocket").unwrap();
        response.insert_header("Connection", "upgrade").unwrap();
        assert!(is_upgrade_resp(&response));

        // wrong http version
        response.set_version(Version::HTTP_10);
        response.insert_header("Upgrade", "websocket").unwrap();
        response.insert_header("Connection", "upgrade").unwrap();
        assert!(!is_upgrade_resp(&response));

        // not 101
        response.set_status(StatusCode::OK).unwrap();
        response.set_version(Version::HTTP_11);
        assert!(!is_upgrade_resp(&response));
    }

    #[test]
    fn test_is_chunked_encoding_from_headers_empty() {
        let empty_headers = HMap::new();
        assert!(!is_chunked_encoding_from_headers(&empty_headers));
    }

    #[rstest]
    #[case::single_chunked("chunked", true)]
    #[case::comma_separated_final("identity, chunked", true)]
    #[case::whitespace_around("  chunked  ", true)]
    #[case::empty_elements_before(", , , chunked", true)]
    #[case::only_identity("identity", false)]
    #[case::trailing_comma("chunked, ", false)]
    #[case::multiple_trailing_commas("chunked, , ", false)]
    #[case::empty_value("", false)]
    #[case::whitespace_only("   ", false)]
    fn test_is_chunked_encoding_single_header(#[case] value: &str, #[case] expected: bool) {
        let mut headers = HMap::new();
        headers.insert(TRANSFER_ENCODING, value.try_into().unwrap());
        assert_eq!(is_chunked_encoding_from_headers(&headers), expected);
    }

    #[rstest]
    #[case::two_headers_chunked_last(&["identity", "chunked"], true)]
    #[case::three_headers_chunked_last(&["gzip", "identity", "chunked"], true)]
    #[case::last_has_comma_separated(&["gzip", "identity, chunked"], true)]
    #[case::whitespace_in_last(&["gzip", "  chunked  "], true)]
    #[case::two_headers_no_chunked(&["identity", "gzip"], false)]
    #[case::chunked_not_last(&["chunked", "identity"], false)]
    #[case::last_has_chunked_not_final(&["gzip", "chunked, identity"], false)]
    #[case::chunked_overridden(&["chunked", "identity, gzip"], false)]
    #[case::trailing_comma_in_last(&["gzip", "chunked, "], false)]
    fn test_is_chunked_encoding_multiple_headers(#[case] values: &[&str], #[case] expected: bool) {
        let mut headers = HMap::new();
        for value in values {
            headers.append(TRANSFER_ENCODING, (*value).try_into().unwrap());
        }
        assert_eq!(is_chunked_encoding_from_headers(&headers), expected);
    }
}
