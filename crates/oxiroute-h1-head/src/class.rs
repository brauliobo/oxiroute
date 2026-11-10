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

/// Bit of the classes the classic writers spell in title case.
pub(crate) const TITLED: u32 = Class::AcceptRanges.bit()
    | Class::Age.bit()
    | Class::CacheControl.bit()
    | Class::Connection.bit()
    | Class::ContentType.bit()
    | Class::ContentEncoding.bit()
    | Class::ContentLength.bit()
    | Class::Date.bit()
    | Class::TransferEncoding.bit()
    | Class::Host.bit()
    | Class::Server.bit()
    | Class::SetCookie.bit();

#[inline(always)]
fn word<const N: usize, const W: usize>(bytes: &[u8; N], at: usize) -> u64 {
    let mut le = [0u8; 8];
    le[..W].copy_from_slice(&bytes[at..at + W]);
    u64::from_le_bytes(le)
}

/// Case-insensitive equality of a token `name` with a lowercase letters-and-hyphens `lit`. `|0x20` folds exactly the
/// ASCII capitals; a token byte can only collide with a hyphen through a CR, which is not one. Whole words are
/// compared, the last overlapping the one before when `N` is not a multiple of the word size.
#[inline(always)]
fn ieq<const N: usize>(name: &[u8], lit: &[u8; N]) -> bool {
    let Ok(name) = <&[u8; N]>::try_from(name) else {
        return false;
    };
    if N >= 8 {
        let mut diff = 0;
        let mut at = 0;
        while at + 8 < N {
            diff |= (word::<N, 8>(name, at) | 0x2020_2020_2020_2020) ^ word::<N, 8>(lit, at);
            at += 8;
        }
        diff |= (word::<N, 8>(name, N - 8) | 0x2020_2020_2020_2020) ^ word::<N, 8>(lit, N - 8);
        diff == 0
    } else if N >= 4 {
        let last = N - 4;
        ((word::<N, 4>(name, 0) | 0x2020_2020) ^ word::<N, 4>(lit, 0))
            | ((word::<N, 4>(name, last) | 0x2020_2020) ^ word::<N, 4>(lit, last))
            == 0
    } else {
        name.iter()
            .zip(lit)
            .fold(0, |diff, (a, b)| diff | ((a | 0x20) ^ b))
            == 0
    }
}

/// The class of a header name made of token bytes, whatever its case.
#[inline]
#[must_use]
pub fn classify(name: &[u8]) -> Class {
    let Some(&first) = name.first() else {
        return Class::Other;
    };
    let found = match (name.len(), first | 0x20) {
        (2, b't') => ieq::<2>(name, b"te").then_some(Class::Te),
        (3, b'a') => ieq::<3>(name, b"age").then_some(Class::Age),
        (4, b'd') => ieq::<4>(name, b"date").then_some(Class::Date),
        (4, b'h') => ieq::<4>(name, b"host").then_some(Class::Host),
        (6, b'e') => ieq::<6>(name, b"expect").then_some(Class::Expect),
        (6, b's') => ieq::<6>(name, b"server").then_some(Class::Server),
        (7, b'u') => ieq::<7>(name, b"upgrade").then_some(Class::Upgrade),
        (7, b't') => ieq::<7>(name, b"trailer").then_some(Class::Trailer),
        (10, b'c') => ieq::<10>(name, b"connection").then_some(Class::Connection),
        (10, b'k') => ieq::<10>(name, b"keep-alive").then_some(Class::KeepAlive),
        (10, b's') => ieq::<10>(name, b"set-cookie").then_some(Class::SetCookie),
        (12, b'c') => ieq::<12>(name, b"content-type").then_some(Class::ContentType),
        (12, b'x') => ieq::<12>(name, b"x-request-id").then_some(Class::XRequestId),
        (13, b'a') => ieq::<13>(name, b"accept-ranges").then_some(Class::AcceptRanges),
        (13, b'c') => ieq::<13>(name, b"cache-control").then_some(Class::CacheControl),
        (14, b'c') => ieq::<14>(name, b"content-length").then_some(Class::ContentLength),
        (16, b'c') => ieq::<16>(name, b"content-encoding").then_some(Class::ContentEncoding),
        (16, b'p') => ieq::<16>(name, b"proxy-connection").then_some(Class::ProxyConnection),
        (17, b't') => ieq::<17>(name, b"transfer-encoding").then_some(Class::TransferEncoding),
        (18, b'p') => ieq::<18>(name, b"proxy-authenticate").then_some(Class::ProxyAuthenticate),
        (19, b'p') => ieq::<19>(name, b"proxy-authorization").then_some(Class::ProxyAuthorization),
        (21, b'x') => {
            ieq::<21>(name, b"x-upstream-request-id").then_some(Class::XUpstreamRequestId)
        }
        _ => None,
    };
    found.unwrap_or(Class::Other)
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
