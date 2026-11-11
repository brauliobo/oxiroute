//! Request heads and trailers: a QPACK field section turned into an `http::Request<()>` or a `HeaderMap`, with the
//! validation of RFC 9114 section 4.2 and 4.3.
use bytes::{Bytes, BytesMut};
use http::header::{HOST, HeaderMap, HeaderName, HeaderValue};
use http::uri::{Authority, PathAndQuery, Scheme, Uri};
use http::{Method, Request};

use crate::qpack::decode::{DecodeError, Line, Section, Str, section};
use crate::qpack::static_table::{ENTRIES, KINDS, Kind, NAMES, VALUES};

/// Why a section did not become a head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The decoded list passed the limit at this size.
    TooLarge(u64),
    /// A malformed message (RFC 9114 section 4.1.2): a stream error.
    Malformed(&'static str),
    /// The section does not decode: a connection error.
    Qpack(DecodeError),
}

impl From<DecodeError> for Failure {
    fn from(error: DecodeError) -> Self {
        Failure::Qpack(error)
    }
}

/// Decodes into one allocation per head: every string is a slice of `arena`.
struct Builder {
    arena: BytesMut,
    scratch: Vec<u8>,
    headers: HeaderMap,
    size: u64,
    limit: u64,
    regular_seen: bool,
}

impl Builder {
    fn new(block_len: usize, limit: u64) -> Self {
        Self {
            arena: BytesMut::with_capacity(block_len + block_len / 2 + 16),
            scratch: Vec::new(),
            headers: HeaderMap::with_capacity((block_len / 12).clamp(8, 64)),
            size: 0,
            limit,
            regular_seen: false,
        }
    }

    /// Counts a field against the limit.
    fn count(&mut self, name: usize, value: usize) -> Result<(), Failure> {
        self.size += (name + value) as u64;
        if self.size > self.limit {
            return Err(Failure::TooLarge(self.size));
        }
        Ok(())
    }

    /// The decoded string as a slice of the arena.
    fn text(&mut self, string: Str<'_>) -> Result<Bytes, Failure> {
        if !string.huffman {
            self.arena.extend_from_slice(string.bytes);
            return Ok(self.arena.split_to(string.bytes.len()).freeze());
        }
        self.scratch.clear();
        string.write_to(&mut self.scratch)?;
        self.arena.extend_from_slice(&self.scratch);
        Ok(self.arena.split_to(self.scratch.len()).freeze())
    }
}

/// Pseudo-header fields of a request.
#[derive(Default)]
struct Pseudo {
    method: Option<Method>,
    scheme: Option<Scheme>,
    authority: Option<Authority>,
    path: Option<PathAndQuery>,
    host: Option<Bytes>,
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), Failure> {
    match slot.replace(value) {
        None => Ok(()),
        Some(_) => Err(Failure::Malformed("repeated pseudo-header field")),
    }
}

fn non_empty(text: &Bytes, what: &'static str) -> Result<(), Failure> {
    if text.is_empty() {
        Err(Failure::Malformed(what))
    } else {
        Ok(())
    }
}

impl Pseudo {
    fn set(&mut self, kind: Kind, value: &Bytes) -> Result<(), Failure> {
        match kind {
            Kind::Method => set_once(
                &mut self.method,
                Method::from_bytes(value).map_err(|_| Failure::Malformed("invalid :method"))?,
            ),
            Kind::Scheme => {
                non_empty(value, "empty :scheme")?;
                let scheme = Scheme::try_from(&value[..])
                    .map_err(|_| Failure::Malformed("invalid :scheme"))?;
                set_once(&mut self.scheme, scheme)
            }
            Kind::Authority => {
                non_empty(value, "empty :authority")?;
                let authority = Authority::from_maybe_shared(value.clone())
                    .map_err(|_| Failure::Malformed("invalid :authority"))?;
                set_once(&mut self.authority, authority)
            }
            Kind::Path => {
                non_empty(value, "empty :path")?;
                let path = PathAndQuery::from_maybe_shared(value.clone())
                    .map_err(|_| Failure::Malformed("invalid :path"))?;
                set_once(&mut self.path, path)
            }
            Kind::Status => Err(Failure::Malformed(":status in a request")),
            Kind::Regular => unreachable!("regular fields are not pseudo-header fields"),
        }
    }

    /// The request target: `:authority` and the `host` field must agree when both are present.
    fn into_uri(self, connect: bool) -> Result<Uri, Failure> {
        if !connect && (self.scheme.is_none() || self.path.is_none()) {
            return Err(Failure::Malformed("missing :scheme or :path"));
        }
        let authority = match (self.authority, self.host) {
            (None, None) => return Err(Failure::Malformed("missing authority")),
            (Some(authority), Some(host)) if authority.as_str().as_bytes() != host => {
                return Err(Failure::Malformed("authority and host differ"));
            }
            (Some(authority), _) => authority,
            (None, Some(host)) => Authority::from_maybe_shared(host)
                .map_err(|_| Failure::Malformed("invalid host"))?,
        };
        let mut parts = http::uri::Parts::default();
        parts.scheme = self.scheme;
        parts.authority = Some(authority);
        parts.path_and_query = self.path;
        Uri::from_parts(parts).map_err(|_| Failure::Malformed("invalid request target"))
    }
}

/// Field names HTTP/3 forbids (RFC 9114 section 4.2).
fn connection_specific(name: &HeaderName, value: &[u8]) -> bool {
    match name.as_str() {
        "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade" => true,
        "te" => value != b"trailers",
        _ => false,
    }
}

impl Builder {
    fn regular(
        &mut self,
        name: HeaderName,
        value: Bytes,
        pseudo: &mut Pseudo,
    ) -> Result<(), Failure> {
        let value = HeaderValue::from_maybe_shared(value)
            .map_err(|_| Failure::Malformed("invalid field value"))?;
        if connection_specific(&name, value.as_bytes()) {
            return Err(Failure::Malformed("connection-specific field"));
        }
        if name == HOST {
            pseudo.host = Some(Bytes::copy_from_slice(value.as_bytes()));
        }
        self.regular_seen = true;
        self.headers.append(name, value);
        Ok(())
    }

    fn line(&mut self, line: Line<'_>, pseudo: &mut Pseudo, trailers: bool) -> Result<(), Failure> {
        match line {
            Line::Indexed(index) => {
                let index = usize::from(index);
                let (name, value) = ENTRIES[index];
                self.count(name.len(), value.len())?;
                match KINDS[index] {
                    Kind::Regular => self.regular_static(index),
                    kind => {
                        let value = Bytes::from_static(value.as_bytes());
                        self.pseudo(kind, &value, pseudo, trailers)?;
                    }
                }
            }
            Line::NameRef { index, value } => {
                let index = usize::from(index);
                let value = self.text(value)?;
                self.count(ENTRIES[index].0.len(), value.len())?;
                match KINDS[index] {
                    Kind::Regular => self.regular(NAMES[index].clone(), value, pseudo)?,
                    kind => self.pseudo(kind, &value, pseudo, trailers)?,
                }
            }
            Line::Literal { name, value } => {
                let name = self.text(name)?;
                let value = self.text(value)?;
                self.count(name.len(), value.len())?;
                if name.first() == Some(&b':') {
                    let kind = pseudo_kind(&name)?;
                    self.pseudo(kind, &value, pseudo, trailers)?;
                } else {
                    let name = HeaderName::from_lowercase(&name)
                        .map_err(|_| Failure::Malformed("invalid field name"))?;
                    self.regular(name, value, pseudo)?;
                }
            }
        }
        Ok(())
    }

    fn regular_static(&mut self, index: usize) {
        self.regular_seen = true;
        self.headers
            .append(NAMES[index].clone(), VALUES[index].clone());
    }

    fn pseudo(
        &mut self,
        kind: Kind,
        value: &Bytes,
        pseudo: &mut Pseudo,
        trailers: bool,
    ) -> Result<(), Failure> {
        if trailers {
            return Err(Failure::Malformed("pseudo-header field in trailers"));
        }
        if self.regular_seen {
            return Err(Failure::Malformed(
                "pseudo-header field after a regular field",
            ));
        }
        pseudo.set(kind, value)
    }
}

fn pseudo_kind(name: &[u8]) -> Result<Kind, Failure> {
    match name {
        b":authority" => Ok(Kind::Authority),
        b":path" => Ok(Kind::Path),
        b":method" => Ok(Kind::Method),
        b":scheme" => Ok(Kind::Scheme),
        b":status" => Ok(Kind::Status),
        _ => Err(Failure::Malformed("unknown pseudo-header field")),
    }
}

fn run(
    builder: &mut Builder,
    mut section: Section<'_>,
    pseudo: &mut Pseudo,
    trailers: bool,
) -> Result<(), Failure> {
    while let Some(line) = section.next_line()? {
        builder.line(line, pseudo, trailers)?;
    }
    Ok(())
}

/// Decodes a request head.
///
/// # Errors
///
/// See [`Failure`].
pub fn request(block: &[u8], limit: u64) -> Result<Request<()>, Failure> {
    let mut builder = Builder::new(block.len(), limit);
    let mut pseudo = Pseudo::default();
    run(&mut builder, section(block)?, &mut pseudo, false)?;
    let method = pseudo
        .method
        .take()
        .ok_or(Failure::Malformed("missing :method"))?;
    let connect = method == Method::CONNECT;
    let uri = pseudo.into_uri(connect)?;
    let mut request = Request::new(());
    *request.method_mut() = method;
    *request.uri_mut() = uri;
    *request.headers_mut() = builder.headers;
    *request.version_mut() = http::Version::HTTP_3;
    Ok(request)
}

/// Decodes a trailer section.
///
/// # Errors
///
/// See [`Failure`].
pub fn trailers(block: &[u8], limit: u64) -> Result<HeaderMap, Failure> {
    let mut builder = Builder::new(block.len(), limit);
    run(&mut builder, section(block)?, &mut Pseudo::default(), true)?;
    Ok(builder.headers)
}
