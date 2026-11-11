//! A [`RequestView`] as `http::request::Parts`. The view is validated when it is decoded, so every conversion
//! below succeeds; the unit tests in `validate` check the validators against `http`.
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::request::Parts;
use http::uri::{Authority, PathAndQuery, Scheme, Uri};
use http::{HeaderMap, Method, Request, Version};

use super::view::{Pseudo, RequestView};

const VALID: &str = "validated when decoded";

/// The previous request's fields by position: clients repeat most of their headers, so a hit clones the
/// reference-counted name and value instead of building them again.
#[derive(Default)]
pub struct FieldCache {
    fields: Vec<(HeaderName, HeaderValue)>,
    authority: Option<Authority>,
    path: Option<PathAndQuery>,
}

const CACHE_FIELDS: usize = 32;
const CACHE_VALUE: usize = 256;

impl FieldCache {
    fn field(&mut self, at: usize, name: &[u8], value: &[u8]) -> (HeaderName, HeaderValue) {
        match self.fields.get(at) {
            Some((n, v)) if n.as_str().as_bytes() == name && v.as_bytes() == value => {
                return (n.clone(), v.clone());
            }
            _ => {}
        }
        let entry = (
            HeaderName::from_lowercase(name).expect(VALID),
            HeaderValue::from_bytes(value).expect(VALID),
        );
        if at < CACHE_FIELDS && value.len() <= CACHE_VALUE {
            if let Some(slot) = self.fields.get_mut(at) {
                slot.clone_from(&entry);
            } else if self.fields.len() == at {
                self.fields.push(entry.clone());
            }
        }
        entry
    }

    fn authority(&mut self, value: &[u8]) -> Authority {
        match &self.authority {
            Some(a) if a.as_str().as_bytes() == value => a.clone(),
            _ => self.authority.insert(authority(value)).clone(),
        }
    }

    fn path(&mut self, value: &[u8]) -> PathAndQuery {
        match &self.path {
            Some(p) if p.as_str().as_bytes() == value => p.clone(),
            _ => self.path.insert(path(value)).clone(),
        }
    }
}

fn authority(value: &[u8]) -> Authority {
    Authority::from_maybe_shared(Bytes::copy_from_slice(value)).expect(VALID)
}

fn path(value: &[u8]) -> PathAndQuery {
    PathAndQuery::from_maybe_shared(Bytes::copy_from_slice(value)).expect(VALID)
}

impl RequestView {
    /// The request head as `http` types, exactly what [`Server::accept`](crate::server::Server::accept) returns.
    #[must_use]
    pub fn into_parts(self) -> Parts {
        self.convert(None)
    }

    /// As [`RequestView::into_parts`], reusing the previous request's equal fields.
    pub(crate) fn into_parts_cached(self, cache: &mut FieldCache) -> Parts {
        self.convert(Some(cache))
    }

    fn convert(&self, mut cache: Option<&mut FieldCache>) -> Parts {
        let mut headers = HeaderMap::with_capacity(self.len());
        for (at, (name, value)) in self.fields().enumerate() {
            let (name, value) = match cache.as_deref_mut() {
                Some(cache) => cache.field(at, name, value),
                None => (
                    HeaderName::from_lowercase(name).expect(VALID),
                    HeaderValue::from_bytes(value).expect(VALID),
                ),
            };
            headers
                .try_append(name, value)
                .expect("at most MAX_FIELDS fields");
        }
        let mut uri = http::uri::Parts::default();
        uri.authority = self.authority().map(|a| match cache.as_deref_mut() {
            Some(cache) => cache.authority(a),
            None => authority(a),
        });
        uri.scheme = self
            .scheme()
            .filter(|_| uri.authority.is_some())
            .map(|s| Scheme::try_from(s).expect(VALID));
        uri.path_and_query = self.optional(Pseudo::Path).map(|p| match cache {
            Some(cache) => cache.path(p),
            None => path(p),
        });
        let (mut parts, ()) = Request::new(()).into_parts();
        parts.method = Method::from_bytes(self.method()).expect(VALID);
        parts.uri = Uri::from_parts(uri).expect(VALID);
        parts.version = Version::HTTP_2;
        parts.headers = headers;
        parts
    }
}
