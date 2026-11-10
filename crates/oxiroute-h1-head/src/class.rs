//! Header names the fast lane reasons about, recognised by length and first byte without hashing.

/// Bit of a hop-by-hop mask: `Keep-Alive` is present.
pub const HOP_KEEP_ALIVE: u16 = 1 << 0;
/// Bit of a hop-by-hop mask: `Proxy-Connection` is present.
pub const HOP_PROXY_CONNECTION: u16 = 1 << 1;
/// Bit of a hop-by-hop mask: `Proxy-Authenticate` is present.
pub const HOP_PROXY_AUTHENTICATE: u16 = 1 << 2;
/// Bit of a hop-by-hop mask: `Proxy-Authorization` is present.
pub const HOP_PROXY_AUTHORIZATION: u16 = 1 << 3;
/// Bit of a hop-by-hop mask: `TE` is present.
pub const HOP_TE: u16 = 1 << 4;
/// Bit of a hop-by-hop mask: `Trailer` is present.
pub const HOP_TRAILER: u16 = 1 << 5;
/// Bit of a response hop-by-hop mask: `Connection` is present.
pub const HOP_CONNECTION: u16 = 1 << 6;
/// Bit of a response hop-by-hop mask: `Transfer-Encoding` is present.
pub const HOP_TRANSFER_ENCODING: u16 = 1 << 7;
/// Bit of a response hop-by-hop mask: `Upgrade` is present.
pub const HOP_UPGRADE: u16 = 1 << 8;
/// The six fixed hop-by-hop fields of a request.
pub const HOP_BY_HOP_REQUEST: u16 = 0x3f;
/// The six fixed fields, `Connection`, `Transfer-Encoding` and `Upgrade` of a response.
pub const HOP_BY_HOP_RESPONSE: u16 = 0x1ff;

/// A header name the lane knows. Everything else is [`Class::Other`].
///
/// The six names of [`HOP_BY_HOP_REQUEST`](crate::HOP_BY_HOP_REQUEST) come first and are consecutive, then `Connection`,
/// `Transfer-Encoding` and `Upgrade`, so a presence mask shifted by one is the hop-by-hop bitmask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    #[default]
    Other = 0,
    KeepAlive,
    ProxyConnection,
    ProxyAuthenticate,
    ProxyAuthorization,
    Te,
    Trailer,
    Connection,
    TransferEncoding,
    Upgrade,
    Host,
    ContentLength,
    Expect,
    XRequestId,
    XUpstreamRequestId,
    Date,
    Server,
    SetCookie,
    CacheControl,
    ContentType,
    ContentEncoding,
    AcceptRanges,
    Age,
}

impl Class {
    /// The bit of this class in a presence mask.
    #[must_use]
    pub const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// The lowercase name, empty for [`Class::Other`].
    #[must_use]
    pub const fn lower(self) -> &'static [u8] {
        match self {
            Self::Other => b"",
            Self::KeepAlive => b"keep-alive",
            Self::ProxyConnection => b"proxy-connection",
            Self::ProxyAuthenticate => b"proxy-authenticate",
            Self::ProxyAuthorization => b"proxy-authorization",
            Self::Te => b"te",
            Self::Trailer => b"trailer",
            Self::Connection => b"connection",
            Self::TransferEncoding => b"transfer-encoding",
            Self::Upgrade => b"upgrade",
            Self::Host => b"host",
            Self::ContentLength => b"content-length",
            Self::Expect => b"expect",
            Self::XRequestId => b"x-request-id",
            Self::XUpstreamRequestId => b"x-upstream-request-id",
            Self::Date => b"date",
            Self::Server => b"server",
            Self::SetCookie => b"set-cookie",
            Self::CacheControl => b"cache-control",
            Self::ContentType => b"content-type",
            Self::ContentEncoding => b"content-encoding",
            Self::AcceptRanges => b"accept-ranges",
            Self::Age => b"age",
        }
    }

    /// The spelling the classic writers give this name: the twelve names `pingora-http` writes in title case, the
    /// same twelve as the vendored response serializer.
    #[must_use]
    pub const fn titled(self) -> Option<&'static [u8]> {
        Some(match self {
            Self::AcceptRanges => b"Accept-Ranges",
            Self::Age => b"Age",
            Self::CacheControl => b"Cache-Control",
            Self::Connection => b"Connection",
            Self::ContentType => b"Content-Type",
            Self::ContentEncoding => b"Content-Encoding",
            Self::ContentLength => b"Content-Length",
            Self::Date => b"Date",
            Self::TransferEncoding => b"Transfer-Encoding",
            Self::Host => b"Host",
            Self::Server => b"Server",
            Self::SetCookie => b"Set-Cookie",
            _ => return None,
        })
    }
}

/// Case-insensitive equality of a token `name` with a lowercase letters-and-hyphens `lower` of the same length.
/// `|0x20` folds exactly the ASCII capitals; a token byte can only collide with a hyphen through a CR, which is not one.
#[inline(always)]
fn eq_lower(name: &[u8], lower: &[u8]) -> bool {
    name.iter()
        .zip(lower)
        .fold(0, |diff, (a, b)| diff | ((a | 0x20) ^ b))
        == 0
}

/// The class of a header name made of token bytes, whatever its case.
#[inline]
#[must_use]
pub fn classify(name: &[u8]) -> Class {
    let Some(&first) = name.first() else {
        return Class::Other;
    };
    let candidate = match (name.len(), first | 0x20) {
        (2, b't') => Class::Te,
        (3, b'a') => Class::Age,
        (4, b'd') => Class::Date,
        (4, b'h') => Class::Host,
        (6, b'e') => Class::Expect,
        (6, b's') => Class::Server,
        (7, b'u') => Class::Upgrade,
        (7, b't') => Class::Trailer,
        (10, b'c') => Class::Connection,
        (10, b'k') => Class::KeepAlive,
        (10, b's') => Class::SetCookie,
        (12, b'c') => Class::ContentType,
        (12, b'x') => Class::XRequestId,
        (13, b'a') => Class::AcceptRanges,
        (13, b'c') => Class::CacheControl,
        (14, b'c') => Class::ContentLength,
        (16, b'c') => Class::ContentEncoding,
        (16, b'p') => Class::ProxyConnection,
        (17, b't') => Class::TransferEncoding,
        (18, b'p') => Class::ProxyAuthenticate,
        (19, b'p') => Class::ProxyAuthorization,
        (21, b'x') => Class::XUpstreamRequestId,
        _ => return Class::Other,
    };
    if eq_lower(name, candidate.lower()) {
        candidate
    } else {
        Class::Other
    }
}

/// Whether two token names are the same header.
#[inline]
pub(crate) fn same_name(a: &[u8], a_class: Class, b: &[u8], b_class: Class) -> bool {
    if a_class == Class::Other && b_class == Class::Other {
        a.eq_ignore_ascii_case(b)
    } else {
        a_class == b_class
    }
}
