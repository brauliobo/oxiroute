use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use http::{HeaderName, HeaderValue, header::Entry};

use crate::observability::logging::valid_correlation_id;

pub(crate) const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
/// Passed to `insert_header` as a static string: a `HeaderName` there is copied once more for a name outside the
/// standard table.
const REQUEST_ID_HEADER_NAME: &str = "x-request-id";
pub(crate) const UPSTREAM_REQUEST_ID_HEADER: HeaderName =
    HeaderName::from_static("x-upstream-request-id");
const DEFAULT_IDEMPOTENCY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Start time and pid hashed to 32 bits, so restarted and parallel workers never share ids.
fn boot_tag() -> u32 {
    static BOOT: OnceLock<u32> = OnceLock::new();
    *BOOT.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let seed = format!("{nanos}:{}", std::process::id());
        u32::try_from(fnv1a(seed.as_bytes()) >> 32).expect("32 bit tag")
    })
}

/// Lowercase hex of `value` into `out`, most significant digit first, zero padded to the width of `out`.
fn write_hex(out: &mut [u8], mut value: u64) {
    for digit in out.iter_mut().rev() {
        *digit = b"0123456789abcdef"[(value & 0xf) as usize];
        value >>= 4;
    }
}

/// `rq-`, boot tag and sequence: fixed width, so it renders without allocating.
fn generated_request_id() -> HeaderValue {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff_ffff;
    let mut id = *b"rq-00000000-000000000000";
    write_hex(&mut id[3..11], u64::from(boot_tag()));
    write_hex(&mut id[12..], sequence);
    HeaderValue::from_bytes(&id).expect("ASCII id")
}

/// Keeps a well formed incoming id; anything else is replaced, never rejected.
pub(crate) fn resolve_request_id(incoming: Option<&HeaderValue>) -> HeaderValue {
    match incoming {
        Some(id) if id.to_str().is_ok_and(valid_correlation_id) => id.clone(),
        _ => generated_request_id(),
    }
}

/// First 8 hex digits of a stable hash: correlates a client re-issue without logging the key.
pub(crate) fn idempotency_tag(value: &[u8]) -> String {
    format!("{:08x}", fnv1a(value) >> 32)
}

pub(crate) fn idempotency_header_or_default(configured: Option<&HeaderName>) -> HeaderName {
    configured.cloned().unwrap_or(DEFAULT_IDEMPOTENCY_HEADER)
}

/// Tags the request before routing, so every attempt and every response, including proxied ones, shares one
/// id; returns the id for the response side.
pub(crate) fn stamp_request(
    request: &mut pingora::http::RequestHeader,
) -> pingora::Result<HeaderValue> {
    let mut incoming = request.headers.get_all(&REQUEST_ID_HEADER).iter();
    let first = incoming.next();
    // A single well formed id is already what the origin should get.
    if let Some(id) = first.filter(|id| id.to_str().is_ok_and(valid_correlation_id))
        && incoming.next().is_none()
    {
        return Ok(id.clone());
    }
    let id = resolve_request_id(first);
    request.insert_header(REQUEST_ID_HEADER_NAME, id.clone())?;
    Ok(id)
}

/// Gives the response the request's id; one the upstream sent moves to `x-upstream-request-id` when it differs.
pub(crate) fn stamp_response(
    response: &mut pingora::http::ResponseHeader,
    id: &HeaderValue,
) -> pingora::Result<()> {
    // A unique value never matches an HPACK table entry; "never indexed" keeps it out of the table, so the
    // HTTP/2 encoder neither inserts it nor evicts the entries the other headers reuse.
    let mut id = id.clone();
    id.set_sensitive(true);
    // The usual response has no id and no case map: one lookup, and no name to parse.
    if !response.has_case()
        && let Entry::Vacant(slot) = response.headers.entry(REQUEST_ID_HEADER)
    {
        slot.insert(id);
        return Ok(());
    }
    if let Some(upstream) = response.remove_header(&REQUEST_ID_HEADER)
        && upstream != id
    {
        response.insert_header(UPSTREAM_REQUEST_ID_HEADER, upstream)?;
    }
    response.insert_header(REQUEST_ID_HEADER_NAME, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_response_id_is_never_indexed_by_hpack_and_replaces_the_upstream_one() {
        let mut request = pingora::http::RequestHeader::build("GET", b"/", None).unwrap();
        let id = stamp_request(&mut request).unwrap();
        let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
        response
            .insert_header("x-request-id", "upstream-1")
            .unwrap();
        stamp_response(&mut response, &id).unwrap();
        let stamped = &response.headers[&REQUEST_ID_HEADER];
        assert!(stamped.is_sensitive());
        assert_eq!(stamped, &request.headers[&REQUEST_ID_HEADER]);
        assert_eq!(response.headers[&UPSTREAM_REQUEST_ID_HEADER], "upstream-1");
    }

    #[test]
    fn a_response_without_an_id_gets_it_with_or_without_case_preservation() {
        let id = HeaderValue::from_static("rq-1");
        for mut response in [
            pingora::http::ResponseHeader::build(200, None).unwrap(),
            pingora::http::ResponseHeader::build_no_case(200, None).unwrap(),
        ] {
            response.insert_header("etag", "\"a\"").unwrap();
            stamp_response(&mut response, &id).unwrap();
            assert_eq!(response.headers.len(), 2);
            assert!(response.headers[&REQUEST_ID_HEADER].is_sensitive());
            assert_eq!(response.headers[&REQUEST_ID_HEADER], "rq-1");
            let mut wire = Vec::new();
            response.header_to_h1_wire(&mut wire);
            assert!(
                String::from_utf8(wire)
                    .unwrap()
                    .to_lowercase()
                    .contains("x-request-id: rq-1")
            );
        }
    }

    #[test]
    fn a_request_keeps_one_valid_id_and_collapses_duplicates() {
        let mut request = pingora::http::RequestHeader::build("GET", b"/", None).unwrap();
        request.append_header("x-request-id", "client-7").unwrap();
        assert_eq!(stamp_request(&mut request).unwrap(), "client-7");
        assert_eq!(
            request.headers.get_all(&REQUEST_ID_HEADER).iter().count(),
            1
        );
        request.append_header("x-request-id", "client-8").unwrap();
        assert_eq!(stamp_request(&mut request).unwrap(), "client-7");
        let ids = request
            .headers
            .get_all(&REQUEST_ID_HEADER)
            .iter()
            .collect::<Vec<_>>();
        assert_eq!(ids, ["client-7"]);
        request.insert_header("x-request-id", "bad id").unwrap();
        let generated = stamp_request(&mut request).unwrap();
        assert!(generated.as_bytes().starts_with(b"rq-"));
        assert_eq!(request.headers[&REQUEST_ID_HEADER], generated);
    }

    #[test]
    fn well_formed_incoming_ids_are_kept() {
        let incoming = HeaderValue::from_static("client-7:a.b_c");
        assert_eq!(resolve_request_id(Some(&incoming)), "client-7:a.b_c");
    }

    #[test]
    fn malformed_or_oversized_incoming_ids_are_replaced() {
        for bad in ["has space", "semi;colon", ""] {
            let value = HeaderValue::from_str(bad).expect("header value");
            assert!(
                resolve_request_id(Some(&value))
                    .as_bytes()
                    .starts_with(b"rq-"),
                "{bad}"
            );
        }
        let long = HeaderValue::from_str(&"a".repeat(65)).expect("header value");
        assert!(
            resolve_request_id(Some(&long))
                .as_bytes()
                .starts_with(b"rq-")
        );
        assert!(resolve_request_id(None).as_bytes().starts_with(b"rq-"));
    }

    #[test]
    fn generated_ids_have_the_documented_shape_and_never_repeat() {
        let first = generated_request_id();
        let second = generated_request_id();
        assert_ne!(first, second);
        let first = first.to_str().expect("ASCII id");
        let parts: Vec<_> = first.split('-').collect();
        assert_eq!((parts[0], parts[1].len(), parts[2].len()), ("rq", 8, 12));
        assert!(valid_correlation_id(first));
    }

    #[test]
    fn generated_ids_render_the_boot_tag_and_the_sequence_as_padded_hex() {
        let id = generated_request_id();
        let text = id.to_str().expect("ASCII id");
        assert_eq!(&text[3..11], format!("{:08x}", boot_tag()));
        assert!(u64::from_str_radix(&text[12..], 16).is_ok());
        let mut digits = [0; 4];
        write_hex(&mut digits, 0xbeef_1234);
        assert_eq!(&digits, b"1234");
        write_hex(&mut digits, 0x2f);
        assert_eq!(&digits, b"002f");
    }

    #[test]
    fn idempotency_tags_are_short_stable_and_hide_the_key() {
        let tag = idempotency_tag(b"order-8841-secret");
        assert_eq!(tag.len(), 8);
        assert_eq!(tag, idempotency_tag(b"order-8841-secret"));
        assert_ne!(tag, idempotency_tag(b"order-8842-secret"));
        assert!(!tag.contains("order"));
    }
}
