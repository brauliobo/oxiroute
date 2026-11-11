//! The QPACK static table (RFC 9204 appendix A) and the lookups of the request decoder and response encoder.
use std::sync::LazyLock;

use http::{HeaderName, HeaderValue};

/// Number of entries.
pub const LEN: usize = 99;

/// `(name, value)` by index.
pub static ENTRIES: [(&str, &str); LEN] = [
    (":authority", ""),
    (":path", "/"),
    ("age", "0"),
    ("content-disposition", ""),
    ("content-length", "0"),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("referer", ""),
    ("set-cookie", ""),
    (":method", "CONNECT"),
    (":method", "DELETE"),
    (":method", "GET"),
    (":method", "HEAD"),
    (":method", "OPTIONS"),
    (":method", "POST"),
    (":method", "PUT"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "103"),
    (":status", "200"),
    (":status", "304"),
    (":status", "404"),
    (":status", "503"),
    ("accept", "*/*"),
    ("accept", "application/dns-message"),
    ("accept-encoding", "gzip, deflate, br"),
    ("accept-ranges", "bytes"),
    ("access-control-allow-headers", "cache-control"),
    ("access-control-allow-headers", "content-type"),
    ("access-control-allow-origin", "*"),
    ("cache-control", "max-age=0"),
    ("cache-control", "max-age=2592000"),
    ("cache-control", "max-age=604800"),
    ("cache-control", "no-cache"),
    ("cache-control", "no-store"),
    ("cache-control", "public, max-age=31536000"),
    ("content-encoding", "br"),
    ("content-encoding", "gzip"),
    ("content-type", "application/dns-message"),
    ("content-type", "application/javascript"),
    ("content-type", "application/json"),
    ("content-type", "application/x-www-form-urlencoded"),
    ("content-type", "image/gif"),
    ("content-type", "image/jpeg"),
    ("content-type", "image/png"),
    ("content-type", "text/css"),
    ("content-type", "text/html; charset=utf-8"),
    ("content-type", "text/plain"),
    ("content-type", "text/plain;charset=utf-8"),
    ("range", "bytes=0-"),
    ("strict-transport-security", "max-age=31536000"),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains",
    ),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains; preload",
    ),
    ("vary", "accept-encoding"),
    ("vary", "origin"),
    ("x-content-type-options", "nosniff"),
    ("x-xss-protection", "1; mode=block"),
    (":status", "100"),
    (":status", "204"),
    (":status", "206"),
    (":status", "302"),
    (":status", "400"),
    (":status", "403"),
    (":status", "421"),
    (":status", "425"),
    (":status", "500"),
    ("accept-language", ""),
    ("access-control-allow-credentials", "FALSE"),
    ("access-control-allow-credentials", "TRUE"),
    ("access-control-allow-headers", "*"),
    ("access-control-allow-methods", "get"),
    ("access-control-allow-methods", "get, post, options"),
    ("access-control-allow-methods", "options"),
    ("access-control-expose-headers", "content-length"),
    ("access-control-request-headers", "content-type"),
    ("access-control-request-method", "get"),
    ("access-control-request-method", "post"),
    ("alt-svc", "clear"),
    ("authorization", ""),
    (
        "content-security-policy",
        "script-src 'none'; object-src 'none'; base-uri 'none'",
    ),
    ("early-data", "1"),
    ("expect-ct", ""),
    ("forwarded", ""),
    ("if-range", ""),
    ("origin", ""),
    ("purpose", "prefetch"),
    ("server", ""),
    ("timing-allow-origin", "*"),
    ("upgrade-insecure-requests", "1"),
    ("user-agent", ""),
    ("x-forwarded-for", ""),
    ("x-frame-options", "deny"),
    ("x-frame-options", "sameorigin"),
];

/// Entry indices per field name, sorted by name for binary search. The first index serves as a name reference.
static GROUPS: [(&str, &[u8]); 52] = [
    (":authority", &[0]),
    (":method", &[15, 16, 17, 18, 19, 20, 21]),
    (":path", &[1]),
    (":scheme", &[22, 23]),
    (
        ":status",
        &[24, 25, 26, 27, 28, 63, 64, 65, 66, 67, 68, 69, 70, 71],
    ),
    ("accept", &[29, 30]),
    ("accept-encoding", &[31]),
    ("accept-language", &[72]),
    ("accept-ranges", &[32]),
    ("access-control-allow-credentials", &[73, 74]),
    ("access-control-allow-headers", &[33, 34, 75]),
    ("access-control-allow-methods", &[76, 77, 78]),
    ("access-control-allow-origin", &[35]),
    ("access-control-expose-headers", &[79]),
    ("access-control-request-headers", &[80]),
    ("access-control-request-method", &[81, 82]),
    ("age", &[2]),
    ("alt-svc", &[83]),
    ("authorization", &[84]),
    ("cache-control", &[36, 37, 38, 39, 40, 41]),
    ("content-disposition", &[3]),
    ("content-encoding", &[42, 43]),
    ("content-length", &[4]),
    ("content-security-policy", &[85]),
    (
        "content-type",
        &[44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54],
    ),
    ("cookie", &[5]),
    ("date", &[6]),
    ("early-data", &[86]),
    ("etag", &[7]),
    ("expect-ct", &[87]),
    ("forwarded", &[88]),
    ("if-modified-since", &[8]),
    ("if-none-match", &[9]),
    ("if-range", &[89]),
    ("last-modified", &[10]),
    ("link", &[11]),
    ("location", &[12]),
    ("origin", &[90]),
    ("purpose", &[91]),
    ("range", &[55]),
    ("referer", &[13]),
    ("server", &[92]),
    ("set-cookie", &[14]),
    ("strict-transport-security", &[56, 57, 58]),
    ("timing-allow-origin", &[93]),
    ("upgrade-insecure-requests", &[94]),
    ("user-agent", &[95]),
    ("vary", &[59, 60]),
    ("x-content-type-options", &[61]),
    ("x-forwarded-for", &[96]),
    ("x-frame-options", &[97, 98]),
    ("x-xss-protection", &[62]),
];

/// What an entry stands for in a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Authority,
    Path,
    Method,
    Scheme,
    Status,
    Regular,
}

impl Kind {
    const fn of(name: &str) -> Kind {
        match name.as_bytes() {
            b":authority" => Kind::Authority,
            b":path" => Kind::Path,
            b":method" => Kind::Method,
            b":scheme" => Kind::Scheme,
            b":status" => Kind::Status,
            _ => Kind::Regular,
        }
    }
}

/// [`Kind`] by index.
pub static KINDS: [Kind; LEN] = {
    let mut kinds = [Kind::Regular; LEN];
    let mut i = 0;
    while i < LEN {
        kinds[i] = Kind::of(ENTRIES[i].0);
        i += 1;
    }
    kinds
};

/// Header names by index (a placeholder at the pseudo-header entries, which are never looked up here).
pub static NAMES: LazyLock<[HeaderName; LEN]> = LazyLock::new(|| {
    std::array::from_fn(|i| match KINDS[i] {
        Kind::Regular => HeaderName::from_static(ENTRIES[i].0),
        _ => HeaderName::from_static("x-pseudo"),
    })
});

/// Header values by index.
pub static VALUES: LazyLock<[HeaderValue; LEN]> =
    LazyLock::new(|| std::array::from_fn(|i| HeaderValue::from_static(ENTRIES[i].1)));

/// Result of looking a field up in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// An entry with this name and value.
    Exact(u8),
    /// An entry with this name.
    Name(u8),
    None,
}

/// Looks up a field by exact name and value, then by name alone.
#[must_use]
pub fn find(name: &[u8], value: &[u8]) -> Found {
    let Ok(group) = GROUPS.binary_search_by(|(known, _)| known.as_bytes().cmp(name)) else {
        return Found::None;
    };
    let indices = GROUPS[group].1;
    indices
        .iter()
        .find(|&&i| ENTRIES[usize::from(i)].1.as_bytes() == value)
        .map_or(Found::Name(indices[0]), |&i| Found::Exact(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_cover_every_entry_once_and_in_order() {
        let mut seen = [false; LEN];
        for (name, indices) in GROUPS {
            for &i in indices {
                assert_eq!(ENTRIES[usize::from(i)].0, name);
                assert!(!std::mem::replace(&mut seen[usize::from(i)], true));
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert!(GROUPS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn rfc_9204_spot_checks() {
        assert_eq!(ENTRIES[0], (":authority", ""));
        assert_eq!(ENTRIES[17], (":method", "GET"));
        assert_eq!(ENTRIES[25], (":status", "200"));
        assert_eq!(ENTRIES[46], ("content-type", "application/json"));
        assert_eq!(ENTRIES[71], (":status", "500"));
        assert_eq!(ENTRIES[98], ("x-frame-options", "sameorigin"));
    }

    #[test]
    fn find_prefers_exact_then_name() {
        assert_eq!(find(b":status", b"200"), Found::Exact(25));
        assert_eq!(find(b":status", b"500"), Found::Exact(71));
        assert_eq!(find(b":status", b"418"), Found::Name(24));
        assert_eq!(find(b"content-type", b"text/css"), Found::Exact(51));
        assert_eq!(find(b"content-type", b"x/y"), Found::Name(44));
        assert_eq!(find(b"x-custom", b""), Found::None);
        assert_eq!(find(b"server", b""), Found::Exact(92));
    }

    #[test]
    fn kinds_and_names_follow_the_entries() {
        assert_eq!(KINDS[0], Kind::Authority);
        assert_eq!(KINDS[1], Kind::Path);
        assert_eq!(KINDS[19], Kind::Method);
        assert_eq!(KINDS[23], Kind::Scheme);
        assert_eq!(KINDS[63], Kind::Status);
        assert_eq!(KINDS[29], Kind::Regular);
        assert_eq!(NAMES[29].as_str(), "accept");
        assert_eq!(VALUES[29], "*/*");
    }
}
