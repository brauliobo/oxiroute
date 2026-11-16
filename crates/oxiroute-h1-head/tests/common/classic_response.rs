//! The classic response parse, from `vendor/pingora-core/src/protocols/http/v1/client.rs` (`read_response`,
//! `parse_resp_buffer`, `validate_response`, `ResponseFacts`).

use super::classic_request::{Classic, Facts, is_hop_by_hop};
use http::header::{self, HeaderName, HeaderValue};
use httparse::Status;
use pingora_http::ResponseHeader;
use std::mem::MaybeUninit;

pub struct ClassicResponse {
    pub head_len: usize,
    pub header: ResponseHeader,
    pub minor: u8,
    pub facts: Facts,
    /// httparse's list, in arrival order, names as sent.
    pub arrival: Vec<(String, Vec<u8>)>,
    pub reason: String,
}

fn is_header_value(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| (b >= 32 && b != 127) || b == b'\t')
}

/// The length the classic path frames the body with when a `Content-Length` is present: every value of every line and
/// list item is digits only and they are all equal (`content_length_parse_all`).
pub fn framing_length(headers: &http::HeaderMap) -> Result<Option<u64>, ()> {
    let mut agreed: Option<u64> = None;
    for value in headers.get_all(header::CONTENT_LENGTH) {
        let line = value.to_str().map_err(|_| ())?;
        for item in line.split(',').map(str::trim) {
            if item.is_empty() || !item.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            let length: u64 = item.parse().map_err(|_| ())?;
            if *agreed.get_or_insert(length) != length {
                return Err(());
            }
        }
    }
    Ok(agreed)
}

pub fn classic_response(buf: &[u8]) -> Result<ClassicResponse, Classic> {
    let mut slots = [const { MaybeUninit::<httparse::Header<'_>>::uninit() }; 256];
    let mut resp = httparse::Response::new(&mut []);
    let mut parser = httparse::ParserConfig::default();
    parser.allow_spaces_after_header_name_in_responses(true);
    parser.allow_obsolete_multiline_headers_in_responses(true);
    let head_len = match parser.parse_response_with_uninit_headers(&mut resp, buf, &mut slots) {
        Ok(Status::Complete(n)) => n,
        Ok(Status::Partial) => return Err(Classic::Partial),
        Err(_) => return Err(Classic::Parse),
    };
    let mut header =
        ResponseHeader::build_no_case(resp.code.unwrap(), Some(resp.headers.len() + 4))
            .map_err(|_| Classic::Build)?;
    let minor = resp.version.unwrap_or(9);
    header.set_version(match minor {
        1 => http::Version::HTTP_11,
        0 => http::Version::HTTP_10,
        _ => http::Version::HTTP_09,
    });
    header
        .set_reason_phrase(resp.reason)
        .map_err(|_| Classic::Build)?;
    let mut facts = Facts::default();
    let mut arrival = Vec::new();
    for h in resp.headers.iter() {
        let name = HeaderName::from_bytes(h.name.as_bytes()).map_err(|_| Classic::Build)?;
        let value = if h.value.contains(&b'\n') {
            pingora_http::header_value_from_slice(h.value)
        } else if is_header_value(h.value) {
            HeaderValue::from_bytes(h.value).map_err(|_| Classic::Build)?
        } else {
            return Err(Classic::Build);
        };
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
        header
            .headers_mut_no_case()
            .expect("built without a case map")
            .append(name, value);
        arrival.push((h.name.to_owned(), h.value.to_vec()));
    }
    if facts.content_lengths > 0 && facts.transfer_encoding {
        header.remove_header(&header::CONTENT_LENGTH);
        facts.content_lengths = 0;
    }
    if facts.content_lengths > 0 && framing_length(&header.headers).is_err() {
        return Err(Classic::Validate);
    }
    let reason = resp.reason.unwrap_or("").to_owned();
    Ok(ClassicResponse {
        head_len,
        header,
        minor,
        facts,
        arrival,
        reason,
    })
}
