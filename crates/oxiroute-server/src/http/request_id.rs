use std::{
    io::Write as _,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use pingora::modules::http::{HttpModule, HttpModuleBuilder, Module};

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

/// `rq-`, boot tag and sequence: fixed width, so it renders without allocating.
fn generated_request_id() -> HeaderValue {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff_ffff;
    let mut id = [0; 24];
    write!(&mut id[..], "rq-{:08x}-{sequence:012x}", boot_tag()).expect("fixed width id");
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

pub(crate) struct RequestIdBuilder;

impl HttpModuleBuilder for RequestIdBuilder {
    fn init(&self) -> Module {
        Box::new(RequestIdModule { id: None })
    }

    fn order(&self) -> i16 {
        // Pingora negates the order to sort, so `i16::MIN` itself would overflow.
        i16::MIN + 1
    }
}

/// Tags the request before routing so every attempt and every response, including proxied ones, shares one id.
struct RequestIdModule {
    id: Option<HeaderValue>,
}

#[async_trait]
impl HttpModule for RequestIdModule {
    async fn request_header_filter(
        &mut self,
        request: &mut pingora::http::RequestHeader,
    ) -> pingora::Result<()> {
        let value = resolve_request_id(request.headers.get(&REQUEST_ID_HEADER));
        request.insert_header(REQUEST_ID_HEADER_NAME, value.clone())?;
        self.id = Some(value);
        Ok(())
    }

    async fn response_header_filter(
        &mut self,
        response: &mut pingora::http::ResponseHeader,
        _end_of_stream: bool,
    ) -> pingora::Result<()> {
        let Some(id) = &self.id else {
            return Ok(());
        };
        if let Some(upstream) = response.headers.remove(&REQUEST_ID_HEADER)
            && upstream != *id
        {
            response
                .headers
                .insert(UPSTREAM_REQUEST_ID_HEADER, upstream);
        }
        response.insert_header(REQUEST_ID_HEADER_NAME, id.clone())?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn idempotency_tags_are_short_stable_and_hide_the_key() {
        let tag = idempotency_tag(b"order-8841-secret");
        assert_eq!(tag.len(), 8);
        assert_eq!(tag, idempotency_tag(b"order-8841-secret"));
        assert_ne!(tag, idempotency_tag(b"order-8842-secret"));
        assert!(!tag.contains("order"));
    }
}
