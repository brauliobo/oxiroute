//! The classic response parse, from `vendor/pingora-core/src/protocols/http/v1/client.rs` (`read_response`,
//! `parse_resp_buffer`, `validate_response`, `ResponseFacts`).

use super::classic_request::{Classic, Facts, buf_to_content_length, is_hop_by_hop};
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
        let value = if h.value.windows(2).any(|w| w == b"\r\n") {
            let mut folded = h.value.to_vec();
            while let Some(at) = folded.windows(2).position(|w| w == b"\r\n") {
                folded[at..at + 2].copy_from_slice(b"  ");
            }
            HeaderValue::from_bytes(&folded).map_err(|_| Classic::Build)?
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
        header.headers.append(name, value);
        arrival.push((h.name.to_owned(), h.value.to_vec()));
    }
    if facts.content_lengths > 0 && facts.transfer_encoding {
        header.remove_header(&header::CONTENT_LENGTH);
        facts.content_lengths = 0;
    }
    if !facts.transfer_encoding && facts.content_lengths > 1 {
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
