//! The bytes of a cached response on an HTTP/1 connection, the ones the classic path writes for it: the stored headers
//! in the order they were stored, with the four a response writer owns written where the stored response has them and
//! after the others when it does not, in the order the classic path adds them: `Age`, `Content-Length`, the request id,
//! `Date`, `Connection`.

use std::time::Duration;

use bytes::{BufMut as _, BytesMut};
use http::{
    HeaderMap, HeaderName, StatusCode,
    header::{AGE, CONNECTION, CONTENT_LENGTH, DATE},
};
use oxiroute_cache::{CachedResponse, FreshHit};
use pingora::{http::titled_header_name_str, protocols::http::date::put_cached_date};

use crate::http::request_id::REQUEST_ID_HEADER;

const CRLF: &[u8] = b"\r\n";

const fn body_forbidden(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    )
}

/// What the lane answers with: a stored response as it is, or the response the cache's compression made of it.
#[derive(Clone, Copy)]
pub(super) struct Representation<'a> {
    pub(super) status: StatusCode,
    pub(super) headers: &'a HeaderMap,
    pub(super) body: &'a [u8],
    pub(super) age: Duration,
}

impl<'a> From<&'a FreshHit> for Representation<'a> {
    fn from(hit: &'a FreshHit) -> Self {
        Self {
            status: hit.status(),
            headers: hit.headers(),
            body: hit.body(),
            age: hit.age(),
        }
    }
}

impl<'a> From<&'a CachedResponse> for Representation<'a> {
    fn from(response: &'a CachedResponse) -> Self {
        Self {
            status: response.status,
            headers: &response.headers,
            body: &response.body,
            age: response.age,
        }
    }
}

/// The body the response carries: none for `HEAD` and for the statuses without one.
pub(super) fn body_sent(served: Representation<'_>, head: bool) -> &[u8] {
    if head || body_forbidden(served.status) {
        &[]
    } else {
        served.body
    }
}

#[derive(Clone, Copy)]
enum Owned {
    Age,
    Length,
    Date,
    Connection,
}

impl Owned {
    fn named(name: &HeaderName) -> Option<Self> {
        match *name {
            AGE => Some(Self::Age),
            CONTENT_LENGTH => Some(Self::Length),
            DATE => Some(Self::Date),
            CONNECTION => Some(Self::Connection),
            _ => None,
        }
    }

    fn put(self, out: &mut BytesMut, served: Representation<'_>, automatic: &str) {
        match self {
            Self::Age => put_age(out, served),
            Self::Length => put_length(out, served),
            Self::Date => put_date(out),
            Self::Connection => put_connection(out, automatic),
        }
    }
}

/// Which of the fields the writer owns the stored headers had.
#[derive(Default)]
struct Written(u8);

impl Written {
    fn has(&self, owned: Owned) -> bool {
        self.0 & (1 << owned as u8) != 0
    }

    fn mark(&mut self, owned: Owned) {
        self.0 |= 1 << owned as u8;
    }

    fn put_missing(
        &self,
        out: &mut BytesMut,
        served: Representation<'_>,
        automatic: &str,
        owned: [Owned; 2],
    ) {
        for field in owned.into_iter().filter(|field| !self.has(*field)) {
            field.put(out, served, automatic);
        }
    }
}

/// The status line, the headers and the blank line of the response to a request with the given id; `automatic` is the
/// `Connection` value.
pub(super) fn put(out: &mut BytesMut, served: Representation<'_>, (id, automatic): (&[u8], &str)) {
    let status = served.status;
    out.put_slice(b"HTTP/1.1 ");
    out.put_slice(status.as_str().as_bytes());
    out.put_u8(b' ');
    out.put_slice(status.canonical_reason().unwrap_or_default().as_bytes());
    out.put_slice(CRLF);
    let mut written = Written::default();
    for (name, value) in served.headers {
        if let Some(owned) = Owned::named(name) {
            owned.put(out, served, automatic);
            written.mark(owned);
        } else {
            put_field(out, name, value.as_bytes());
        }
    }
    written.put_missing(out, served, automatic, [Owned::Age, Owned::Length]);
    put_field(out, &REQUEST_ID_HEADER, id);
    written.put_missing(out, served, automatic, [Owned::Date, Owned::Connection]);
    out.put_slice(CRLF);
}

fn put_field(out: &mut BytesMut, name: &HeaderName, value: &[u8]) {
    out.put_slice(
        titled_header_name_str(name)
            .unwrap_or(name.as_str())
            .as_bytes(),
    );
    out.put_slice(b": ");
    out.put_slice(value);
    out.put_slice(CRLF);
}

fn put_age(out: &mut BytesMut, served: Representation<'_>) {
    out.put_slice(b"Age: ");
    put_decimal(out, served.age.as_secs());
    out.put_slice(CRLF);
}

fn put_length(out: &mut BytesMut, served: Representation<'_>) {
    if !body_forbidden(served.status) {
        out.put_slice(b"Content-Length: ");
        put_decimal(out, served.body.len() as u64);
        out.put_slice(CRLF);
    }
}

fn put_date(out: &mut BytesMut) {
    out.put_slice(b"Date: ");
    put_cached_date(out);
    out.put_slice(CRLF);
}

fn put_connection(out: &mut BytesMut, automatic: &str) {
    out.put_slice(b"Connection: ");
    out.put_slice(automatic.as_bytes());
    out.put_slice(CRLF);
}

fn put_decimal(out: &mut BytesMut, mut value: u64) {
    let mut digits = [0_u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(value % 10).expect("a remainder of ten is a digit");
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.put_slice(&digits[start..]);
}

#[cfg(test)]
mod tests;
