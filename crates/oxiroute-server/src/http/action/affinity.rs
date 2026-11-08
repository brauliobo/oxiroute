#[allow(clippy::wildcard_imports)]
use super::*;

/// What a request offers to pin it to a server.
pub(crate) struct AffinityRequest<'a> {
    pub(crate) headers: &'a HeaderMap,
    pub(crate) client_ip: Option<std::net::IpAddr>,
    pub(crate) path: &'a str,
}

/// Sticky sessions compiled for one route.
#[derive(Clone, Debug)]
pub(crate) enum AffinityPlan {
    Cookie { name: String, attributes: String },
    Hash(AffinityKey),
    Header(HeaderName),
}

#[derive(Clone, Debug)]
pub(crate) enum AffinityKey {
    Header(HeaderName),
    ClientIp,
    PathPrefix(usize),
}

impl AffinityPlan {
    pub(crate) fn compile(affinity: &HttpAffinity) -> Self {
        let header = |name: &str| {
            HeaderName::from_bytes(name.as_bytes()).expect("validated affinity header")
        };
        match affinity {
            HttpAffinity::Cookie {
                name,
                ttl_s,
                same_site,
                secure,
                http_only,
            } => {
                let mut attributes = format!("; Max-Age={ttl_s}; Path=/");
                attributes.push_str(match same_site {
                    HttpSameSite::Strict => "; SameSite=Strict",
                    HttpSameSite::Lax => "; SameSite=Lax",
                    HttpSameSite::None => "; SameSite=None",
                });
                if *secure {
                    attributes.push_str("; Secure");
                }
                if *http_only {
                    attributes.push_str("; HttpOnly");
                }
                Self::Cookie {
                    name: name.clone(),
                    attributes,
                }
            }
            HttpAffinity::Hash { key } => Self::Hash(
                match HttpAffinityKey::parse(key).expect("validated affinity key") {
                    HttpAffinityKey::Header(name) => AffinityKey::Header(header(&name)),
                    HttpAffinityKey::ClientIp => AffinityKey::ClientIp,
                    HttpAffinityKey::PathPrefix(segments) => {
                        AffinityKey::PathPrefix(usize::from(segments))
                    }
                },
            ),
            HttpAffinity::Header { name } => Self::Header(header(name)),
        }
    }

    /// The selectable server this request is pinned to, if it names one.
    pub(crate) fn preferred(
        &self,
        pool: &RoundRobinPool,
        request: &AffinityRequest<'_>,
    ) -> Option<String> {
        match self {
            Self::Cookie { name, .. } => {
                pool.server_with_cookie_id(parse_cookie_id(&cookie_value(request.headers, name)?)?)
            }
            Self::Header(name) => pool.selectable_server(request.headers.get(name)?.to_str().ok()?),
            Self::Hash(key) => pool.rendezvous_server(&key.bytes(request)?),
        }
    }

    /// The `Set-Cookie` that pins the client to `server`, unless the request already carries it.
    pub(crate) fn set_cookie(
        &self,
        pool: &RoundRobinPool,
        server: &str,
        request: &HeaderMap,
    ) -> Option<HeaderValue> {
        let Self::Cookie { name, attributes } = self else {
            return None;
        };
        let id = format!("{:016x}", pool.cookie_id(server)?);
        if cookie_value(request, name).as_deref() == Some(id.as_str()) {
            return None;
        }
        HeaderValue::from_str(&format!("{name}={id}{attributes}")).ok()
    }
}

impl AffinityKey {
    fn bytes(&self, request: &AffinityRequest<'_>) -> Option<Vec<u8>> {
        match self {
            Self::Header(name) => Some(request.headers.get(name)?.as_bytes().to_vec()),
            Self::ClientIp => Some(request.client_ip?.to_string().into_bytes()),
            Self::PathPrefix(segments) => {
                let segments = request.path.split('/').skip(1).take(*segments);
                Some(segments.collect::<Vec<_>>().join("/").into_bytes())
            }
        }
    }
}

/// The 16 lowercase hex digits of a cookie value that names a server.
fn parse_cookie_id(value: &str) -> Option<u64> {
    let valid = value.len() == 16
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    valid.then(|| u64::from_str_radix(value, 16).ok()).flatten()
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(';'))
        .find_map(|pair| {
            let (found, value) = pair.trim().split_once('=')?;
            (found == name).then(|| value.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefix_keys_use_the_leading_segments() {
        let key = AffinityKey::PathPrefix(2);
        let headers = HeaderMap::new();
        let bytes = |path| {
            key.bytes(&AffinityRequest {
                headers: &headers,
                client_ip: None,
                path,
            })
        };
        assert_eq!(bytes("/a/b/c/d"), bytes("/a/b/z"));
        assert_ne!(bytes("/a/b/c"), bytes("/a/x/c"));
    }

    #[test]
    fn only_sixteen_lowercase_hex_digits_name_a_server() {
        assert_eq!(parse_cookie_id("00000000000000ff"), Some(255));
        for invalid in [
            "",
            "ff",
            "00000000000000FF",
            "+0000000000000ff",
            "000000000000000g",
            "00000000000000ff0",
        ] {
            assert_eq!(parse_cookie_id(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn cookie_values_are_found_among_other_cookies() {
        let mut headers = HeaderMap::new();
        headers.append(
            http::header::COOKIE,
            HeaderValue::from_static("a=1; sid=xyz"),
        );
        headers.append(http::header::COOKIE, HeaderValue::from_static("b=2"));
        assert_eq!(cookie_value(&headers, "sid").as_deref(), Some("xyz"));
        assert_eq!(cookie_value(&headers, "b").as_deref(), Some("2"));
        assert_eq!(cookie_value(&headers, "missing"), None);
    }
}
