//! Setting and adding headers without the detour through a name that is parsed again.
//!
//! `insert_header` and `append_header` of the Pingora header types take the name as text, so a `HeaderName`
//! passed to them is copied into a case-preserving name and parsed back (two allocations for any name outside
//! the standard table). A header without a case map, which is every parsed request and response, only has the map
//! to change, and the name is already the right type.

use http::{HeaderMap, HeaderName, HeaderValue};
use pingora::http::{RequestHeader, ResponseHeader};

pub(crate) trait HeaderOps {
    /// The header map when the header has no case map to keep in step.
    fn header_map_no_case(&mut self) -> Option<&mut HeaderMap>;
    fn insert_with_case(&mut self, name: HeaderName, value: HeaderValue) -> pingora::Result<()>;
    fn append_with_case(&mut self, name: HeaderName, value: HeaderValue) -> pingora::Result<()>;

    /// Replaces every value under `name`, as `insert_header` does.
    fn set_header(&mut self, name: HeaderName, value: HeaderValue) -> pingora::Result<()> {
        match self.header_map_no_case() {
            Some(map) => {
                map.insert(name, value);
                Ok(())
            }
            None => self.insert_with_case(name, value),
        }
    }

    /// Adds a value under `name`, as `append_header` does.
    fn add_header(&mut self, name: HeaderName, value: HeaderValue) -> pingora::Result<()> {
        match self.header_map_no_case() {
            Some(map) => {
                map.append(name, value);
                Ok(())
            }
            None => self.append_with_case(name, value),
        }
    }
}

macro_rules! header_ops {
    ($header:ty) => {
        impl HeaderOps for $header {
            fn header_map_no_case(&mut self) -> Option<&mut HeaderMap> {
                self.headers_mut_no_case()
            }

            fn insert_with_case(
                &mut self,
                name: HeaderName,
                value: HeaderValue,
            ) -> pingora::Result<()> {
                self.insert_header(name, value)
            }

            fn append_with_case(
                &mut self,
                name: HeaderName,
                value: HeaderValue,
            ) -> pingora::Result<()> {
                self.append_header(name, value).map(drop)
            }
        }
    };
}

header_ops!(RequestHeader);
header_ops!(ResponseHeader);

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(header: &RequestHeader) -> String {
        let mut out = Vec::new();
        header.header_to_h1_wire(&mut out);
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn both_header_kinds_set_and_add_like_the_pingora_calls() {
        for mut request in [
            RequestHeader::build("GET", b"/", None).expect("with case"),
            RequestHeader::build_no_case("GET", b"/", None).expect("without case"),
        ] {
            let name = HeaderName::from_static("x-custom");
            request
                .set_header(name.clone(), HeaderValue::from_static("a"))
                .expect("set");
            request
                .add_header(name.clone(), HeaderValue::from_static("b"))
                .expect("add");
            request
                .set_header(
                    HeaderName::from_static("x-other"),
                    HeaderValue::from_static("c"),
                )
                .expect("set");
            assert_eq!(request.headers.get_all(&name).iter().count(), 2);
            request
                .set_header(name.clone(), HeaderValue::from_static("d"))
                .expect("replace");
            assert_eq!(request.headers.get_all(&name).iter().count(), 1);
            assert_eq!(request.headers[&name], "d");
            assert!(wire(&request).to_lowercase().contains("x-custom: d"));
        }
    }
}
