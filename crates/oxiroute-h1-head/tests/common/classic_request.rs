//! The classic request parse, from `vendor/pingora-core/src/protocols/http/v1/server.rs` (`read_request`,
//! `parse_req_buffer`, `validate_request`) and `common.rs` (`RequestFacts`, `is_chunked_encoding_from_headers`).

use http::header::{self, HeaderName, HeaderValue};
use httparse::Status;
use pingora_http::RequestHeader;
use std::mem::MaybeUninit;

pub const CLASSIC_MAX_HEADERS: usize = 256;

#[derive(Debug)]
pub enum Classic {
    Partial,
    /// httparse refused the first attempt (the classic path may still rewrite the request line and retry).
    Parse,
    /// `RequestHeader::build_no_case`, a header name or a validation refused it.
    Build,
    Validate,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub transfer_encoding: bool,
    pub content_lengths: u8,
    pub connection: bool,
    pub upgrade: bool,
    pub hop_by_hop: bool,
}

pub struct ClassicRequest {
    pub head_len: usize,
    pub header: RequestHeader,
    pub minor: u8,
    pub facts: Facts,
    /// httparse's list, in arrival order, names as sent.
    pub arrival: Vec<(String, Vec<u8>)>,
}

pub fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        *name,
        header::TE | header::TRAILER | header::PROXY_AUTHENTICATE | header::PROXY_AUTHORIZATION
    ) || matches!(name.as_str(), "keep-alive" | "proxy-connection")
}

fn find_last_te_token(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .rposition(|&b| b == b',')
        .map_or(bytes, |pos| &bytes[pos + 1..])
        .trim_ascii()
}

pub fn is_chunked_encoding_from_headers(headers: &http::HeaderMap) -> bool {
    let Some(last) = headers
        .get_all(header::TRANSFER_ENCODING)
        .into_iter()
        .next_back()
    else {
        return false;
    };
    let bytes = last.as_bytes();
    bytes.eq_ignore_ascii_case(b"chunked")
        || find_last_te_token(bytes).eq_ignore_ascii_case(b"chunked")
}

pub fn buf_to_content_length(value: &[u8]) -> Option<u64> {
    let n: i64 = std::str::from_utf8(value).ok()?.parse().ok()?;
    u64::try_from(n).ok()
}

pub fn classic_request(buf: &[u8]) -> Result<ClassicRequest, Classic> {
    let mut slots = [const { MaybeUninit::<httparse::Header<'_>>::uninit() }; CLASSIC_MAX_HEADERS];
    let mut req = httparse::Request::new(&mut []);
    let head_len = match req.parse_with_uninit_headers(buf, &mut slots) {
        Ok(Status::Complete(n)) => n,
        Ok(Status::Partial) => return Err(Classic::Partial),
        Err(_) => return Err(Classic::Parse),
    };
    let mut header = RequestHeader::build_no_case(
        req.method.unwrap_or(""),
        req.path.unwrap_or("").as_bytes(),
        Some(req.headers.len() + 4),
    )
    .map_err(|_| Classic::Build)?;
    let minor = req.version.unwrap_or(9);
    header.set_version(match minor {
        1 => http::Version::HTTP_11,
        0 => http::Version::HTTP_10,
        _ => http::Version::HTTP_09,
    });
    let mut facts = Facts::default();
    let mut arrival = Vec::new();
    for h in req.headers.iter() {
        let name = HeaderName::from_bytes(h.name.as_bytes()).map_err(|_| Classic::Build)?;
        let value = HeaderValue::from_bytes(h.value).map_err(|_| Classic::Build)?;
        match &name {
            n if *n == header::CONTENT_LENGTH => {
                facts.content_lengths = (facts.content_lengths + 1).min(2);
            }
            n if *n == header::TRANSFER_ENCODING => facts.transfer_encoding = true,
            n if *n == header::CONNECTION => facts.connection = true,
            n if *n == header::UPGRADE => facts.upgrade = true,
            n if is_hop_by_hop(n) => facts.hop_by_hop = true,
            _ => {}
        }
        header.headers.append(name, value);
        arrival.push((h.name.to_owned(), h.value.to_vec()));
    }
    if facts.content_lengths > 0 && facts.transfer_encoding {
        header.headers.remove(header::CONTENT_LENGTH);
        facts.content_lengths = 0;
    }
    if !facts.transfer_encoding && facts.content_lengths > 1 {
        return Err(Classic::Validate);
    }
    if facts.transfer_encoding && (minor == 0 || !is_chunked_encoding_from_headers(&header.headers))
    {
        return Err(Classic::Validate);
    }
    if facts.content_lengths > 0 {
        let value = header
            .headers
            .get(header::CONTENT_LENGTH)
            .map(HeaderValue::as_bytes);
        if value.and_then(buf_to_content_length).is_none() {
            return Err(Classic::Validate);
        }
    }
    Ok(ClassicRequest {
        head_len,
        header,
        minor,
        facts,
        arrival,
    })
}
