use std::{cmp::Ordering, fmt, str::FromStr};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, Method, uri::Authority};

use crate::http::trim_ows;

/// Borrowed canonical cache-key inputs. `query` excludes the leading `?`.
#[derive(Clone, Copy)]
pub struct RequestKeyInput<'a> {
    pub method: &'a Method,
    pub scheme: &'a str,
    pub authority: &'a str,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub headers: &'a HeaderMap,
}

/// Parsed response `Vary` selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Vary {
    Any,
    Names(Vec<HeaderName>),
}

impl Vary {
    /// Parses all `Vary` field lines, sorting and deduplicating field names.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed field names, empty list elements, or configured bounds.
    pub fn parse(
        headers: &HeaderMap,
        max_fields: usize,
        max_bytes: usize,
    ) -> Result<Self, KeyError> {
        let mut names = Vec::new();
        let mut bytes = 0usize;
        for value in headers.get_all(http::header::VARY) {
            for raw in value.as_bytes().split(|byte| *byte == b',') {
                let raw = trim_ows(raw);
                if raw == b"*" {
                    return Ok(Self::Any);
                }
                if raw.is_empty() {
                    return Err(KeyError::InvalidVary);
                }
                bytes = bytes.checked_add(raw.len()).ok_or(KeyError::TooLarge)?;
                if bytes > max_bytes {
                    return Err(KeyError::TooLarge);
                }
                let name = HeaderName::from_bytes(raw).map_err(|_| KeyError::InvalidVary)?;
                names.push(name);
                if names.len() > max_fields {
                    return Err(KeyError::TooManyVaryFields);
                }
            }
        }
        names.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
        names.dedup();
        Ok(Self::Names(names))
    }

    #[must_use]
    pub fn names(&self) -> Option<&[HeaderName]> {
        match self {
            Self::Any => None,
            Self::Names(names) => Some(names),
        }
    }
}

/// Canonical request identity before `Vary` request fields are applied.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BaseKey {
    pub(crate) method: Method,
    pub(crate) scheme: String,
    pub(crate) authority: String,
    pub(crate) path: String,
    pub(crate) query: Option<String>,
}

impl BaseKey {
    /// Builds a canonical base key. HEAD and GET deliberately share the GET representation key.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid scheme, authority, path, or an oversized key.
    pub fn new(input: RequestKeyInput<'_>, max_bytes: usize) -> Result<Self, KeyError> {
        if input.path.is_empty()
            || !input.path.starts_with('/')
            || input.path.contains(['?', '#'])
            || !input.path.bytes().all(is_uri_byte)
        {
            return Err(KeyError::InvalidPath);
        }
        if input
            .query
            .is_some_and(|query| query.contains('#') || !query.bytes().all(is_uri_byte))
        {
            return Err(KeyError::InvalidQuery);
        }
        let scheme = input.scheme.to_ascii_lowercase();
        if scheme.is_empty()
            || !scheme
                .bytes()
                .enumerate()
                .all(|(index, byte)| is_scheme_byte(byte, index == 0))
        {
            return Err(KeyError::InvalidScheme);
        }
        let authority = canonical_authority(input.authority, &scheme)?;
        let method = if *input.method == Method::HEAD {
            Method::GET
        } else {
            input.method.clone()
        };
        let key = Self {
            method,
            scheme,
            authority,
            path: input.path.to_owned(),
            query: input.query.map(str::to_owned),
        };
        if key.encoded_len() > max_bytes {
            return Err(KeyError::TooLarge);
        }
        Ok(key)
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.method.as_str().len()
            + self.scheme.len()
            + self.authority.len()
            + self.path.len()
            + self.query.as_ref().map_or(0, String::len)
            + 5 * size_of::<u32>()
    }

    pub(crate) fn is_get(&self) -> bool {
        self.method == Method::GET
    }
}

/// Full canonical representation key, including normalized `Vary` request fields.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CacheKey {
    pub(crate) base: BaseKey,
    pub(crate) vary: Vec<VaryValue>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct VaryValue {
    pub(crate) name: HeaderName,
    pub(crate) present: bool,
    pub(crate) value: Bytes,
}

impl Ord for VaryValue {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.name.as_str(), self.present, &self.value).cmp(&(
            other.name.as_str(),
            other.present,
            &other.value,
        ))
    }
}

impl PartialOrd for VaryValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl CacheKey {
    /// Applies a concrete `Vary` set to a base request key.
    ///
    /// # Errors
    ///
    /// Returns an error for `Vary: *` or a normalized key larger than `max_bytes`.
    pub fn new(
        base: BaseKey,
        request_headers: &HeaderMap,
        vary: &Vary,
        max_bytes: usize,
    ) -> Result<Self, KeyError> {
        let names = vary.names().ok_or(KeyError::VaryAny)?;
        let mut values = Vec::with_capacity(names.len());
        for name in names {
            let all = request_headers.get_all(name);
            let present = all.iter().next().is_some();
            let mut normalized = Vec::new();
            for (index, value) in all.iter().enumerate() {
                if index != 0 {
                    normalized.push(b',');
                }
                normalize_field_value(value.as_bytes(), &mut normalized);
            }
            values.push(VaryValue {
                name: name.clone(),
                present,
                value: Bytes::from(normalized),
            });
        }
        let key = Self { base, vary: values };
        if key.encoded_len() > max_bytes {
            return Err(KeyError::TooLarge);
        }
        Ok(key)
    }

    #[must_use]
    pub const fn base(&self) -> &BaseKey {
        &self.base
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.vary
            .iter()
            .fold(self.base.encoded_len(), |size, value| {
                size.saturating_add(value.name.as_str().len())
                    .saturating_add(value.value.len())
                    .saturating_add(1 + 2 * size_of::<u32>())
            })
    }

    pub(crate) fn matches_request(
        &self,
        base: &BaseKey,
        headers: &HeaderMap,
        max_bytes: usize,
    ) -> bool {
        if &self.base != base {
            return false;
        }
        let vary = Vary::Names(self.vary.iter().map(|value| value.name.clone()).collect());
        Self::new(base.clone(), headers, &vary, max_bytes).is_ok_and(|key| key == *self)
    }

    pub(crate) fn same_vary_schema(&self, other: &Self) -> bool {
        self.vary.len() == other.vary.len()
            && self
                .vary
                .iter()
                .zip(&other.vary)
                .all(|(left, right)| left.name == right.name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyError {
    #[error("invalid URI scheme")]
    InvalidScheme,
    #[error("invalid URI authority")]
    InvalidAuthority,
    #[error("cache path must be an absolute path without query or fragment")]
    InvalidPath,
    #[error("cache query must not contain a fragment")]
    InvalidQuery,
    #[error("invalid Vary field")]
    InvalidVary,
    #[error("Vary: * cannot produce a reusable cache key")]
    VaryAny,
    #[error("too many Vary fields")]
    TooManyVaryFields,
    #[error("cache key exceeds configured bounds")]
    TooLarge,
}

impl fmt::Display for RequestKeyInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}://{}{}",
            self.method, self.scheme, self.authority, self.path
        )?;
        if let Some(query) = self.query {
            write!(formatter, "?{query}")?;
        }
        Ok(())
    }
}

fn canonical_authority(raw: &str, scheme: &str) -> Result<String, KeyError> {
    let authority = Authority::from_str(raw).map_err(|_| KeyError::InvalidAuthority)?;
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    match authority.port_u16() {
        Some(80) if scheme == "http" => Ok(host),
        Some(443) if scheme == "https" => Ok(host),
        Some(port) => Ok(format!("{host}:{port}")),
        None => Ok(host),
    }
}

const fn is_scheme_byte(byte: u8, first: bool) -> bool {
    byte.is_ascii_alphabetic()
        || (!first && (byte.is_ascii_digit() || byte == b'+' || byte == b'-' || byte == b'.'))
}

const fn is_uri_byte(byte: u8) -> bool {
    byte >= b'!' && byte <= b'~'
}

fn normalize_field_value(value: &[u8], output: &mut Vec<u8>) {
    let mut whitespace = false;
    for byte in trim_ows(value) {
        if matches!(byte, b' ' | b'\t') {
            whitespace = true;
        } else {
            if whitespace && !output.is_empty() && output.last() != Some(&b',') {
                output.push(b' ');
            }
            output.push(*byte);
            whitespace = false;
        }
    }
}
