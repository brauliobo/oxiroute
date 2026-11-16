//! Response content checks of `r2tool load`: body fill, gzip integrity, header values, presence and absence.

use std::io::Read as _;

use hyper::header::HeaderMap;

/// Content checks beyond status and length: every body byte equals `fill`, a header has an exact value, a header is
/// present with any value, a header is absent. `--expect-gzip-bytes N` validates gzip integrity and N decoded bytes
/// before checking the fill byte. A proxy with the right size but wrong content, or incorrect header rules, counts as
/// errors.
#[derive(Default)]
pub struct Checks {
    pub fill: Option<u8>,
    pub gzip_bytes: Option<usize>,
    pub header: Option<(String, String)>,
    pub present: Option<String>,
    pub absent: Option<String>,
}

impl Checks {
    pub fn body(&self, bytes: &[u8], encoding: Option<&str>) -> bool {
        if let Some(length) = self.gzip_bytes {
            if !encoding.is_some_and(|encoding| encoding.eq_ignore_ascii_case("gzip")) {
                return false;
            }
            let mut decoded = Vec::new();
            let result = flate2::read::MultiGzDecoder::new(bytes).take(length as u64 + 1).read_to_end(&mut decoded);
            return result.is_ok() && decoded.len() == length
                && self.fill.is_none_or(|byte| decoded.iter().all(|&found| found == byte));
        }
        self.fill.is_none_or(|byte| bytes.iter().all(|&found| found == byte))
    }

    pub fn h1(&self, buf: &[u8], head_end: usize) -> bool {
        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
        self.body(&buf[head_end..], header_value(&head, "content-encoding:"))
            && self.header.as_ref().is_none_or(|(name, value)| header_value(&head, &format!("{name}:")) == Some(value))
            && self.present.as_ref().is_none_or(|name| header_value(&head, &format!("{name}:")).is_some())
            && self.absent.as_ref().is_none_or(|name| header_value(&head, &format!("{name}:")).is_none())
    }

    pub fn h2(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        self.body(body, headers.get("content-encoding").and_then(|value| value.to_str().ok()))
            && self.header.as_ref().is_none_or(|(name, value)| {
                headers.get(name.as_str()).and_then(|found| found.to_str().ok()) == Some(value)
            })
            && self.present.as_ref().is_none_or(|name| headers.contains_key(name.as_str()))
            && self.absent.as_ref().is_none_or(|name| !headers.contains_key(name.as_str()))
    }
}

pub fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .find_map(|line| line.strip_prefix(name))
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use hyper::header::HeaderValue;

    use super::*;

    const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nX-Served-By: bench\r\ncontent-length: 4\r\n\r\nxxxx";
    const HEAD_END: usize = RESPONSE.len() - 4;

    fn checks(fill: Option<u8>, header: Option<(&str, &str)>, absent: Option<&str>) -> Checks {
        Checks {
            fill,
            gzip_bytes: None,
            header: header.map(|(name, value)| (name.to_owned(), value.to_owned())),
            absent: absent.map(str::to_owned),
            ..Checks::default()
        }
    }

    #[test]
    fn gzip_checks_require_exact_decoded_bytes_length_and_integrity() {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"xxxx").expect("encode fixture");
        let mut bytes = encoder.finish().expect("gzip fixture");
        let valid = Checks { fill: Some(b'x'), gzip_bytes: Some(4), ..Checks::default() };
        assert!(valid.body(&bytes, Some("gzip")));
        assert!(!valid.body(&bytes, None));
        assert!(!valid.body(&bytes[..bytes.len() - 1], Some("gzip")));
        assert!(!Checks { fill: Some(b'y'), gzip_bytes: Some(4), ..Checks::default() }.body(&bytes, Some("gzip")));
        assert!(!Checks { fill: Some(b'x'), gzip_bytes: Some(3), ..Checks::default() }.body(&bytes, Some("gzip")));
        let checksum = bytes.len() - 8;
        bytes[checksum] ^= 1;
        assert!(!valid.body(&bytes, Some("gzip")));
        assert!(!valid.body(b"invalid gzip", Some("gzip")));
    }

    #[test]
    fn h1_checks_pass_and_fail() {
        assert!(checks(Some(b'x'), Some(("x-served-by", "bench")), Some("content-type")).h1(RESPONSE, HEAD_END));
        assert!(!checks(Some(b'y'), None, None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, Some(("x-served-by", "other")), None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, Some(("x-missing", "bench")), None).h1(RESPONSE, HEAD_END));
        assert!(!checks(None, None, Some("content-length")).h1(RESPONSE, HEAD_END));
    }

    #[test]
    fn header_presence_ignores_the_value() {
        let present = |name: &str| Checks { present: Some(name.to_owned()), ..Checks::default() };
        assert!(present("x-served-by").h1(RESPONSE, HEAD_END));
        assert!(!present("age").h1(RESPONSE, HEAD_END));
        let mut headers = HeaderMap::new();
        headers.insert("x-served-by", HeaderValue::from_static("bench"));
        assert!(present("x-served-by").h2(&headers, b""));
        assert!(!present("age").h2(&headers, b""));
    }

    #[test]
    fn h2_checks_pass_and_fail() {
        let mut headers = HeaderMap::new();
        headers.insert("x-served-by", HeaderValue::from_static("bench"));
        assert!(checks(Some(b'x'), Some(("x-served-by", "bench")), Some("content-type")).h2(&headers, b"xxxx"));
        assert!(!checks(Some(b'x'), None, None).h2(&headers, b"xxyx"));
        assert!(!checks(None, Some(("x-served-by", "other")), None).h2(&headers, b""));
        assert!(!checks(None, None, Some("x-served-by")).h2(&headers, b""));
    }
}
