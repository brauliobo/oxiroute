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
        let servers = pool.selectable_servers();
        match self {
            Self::Cookie { name, .. } => {
                let id = cookie_value(request.headers, name)?;
                servers
                    .into_iter()
                    .map(|(server, _)| server)
                    .find(|server| server_id(pool.name(), server) == id)
            }
            Self::Header(name) => {
                let wanted = request.headers.get(name)?.to_str().ok()?;
                servers
                    .into_iter()
                    .map(|(server, _)| server)
                    .find(|server| server == wanted)
            }
            Self::Hash(key) => rendezvous(&servers, &key.bytes(request)?),
        }
    }

    /// The `Set-Cookie` that pins the client to `server`, unless the request already carries it.
    pub(crate) fn set_cookie(
        &self,
        pool: &str,
        server: &str,
        request: &HeaderMap,
    ) -> Option<HeaderValue> {
        let Self::Cookie { name, attributes } = self else {
            return None;
        };
        let id = server_id(pool, server);
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

/// Opaque cookie value naming one server of one pool; stable across reloads and workers.
fn server_id(pool: &str, server: &str) -> String {
    sha256(format!("oxiroute-affinity\0{pool}\0{server}").as_bytes())[..8]
        .iter()
        .fold(String::with_capacity(16), |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        })
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

/// Weighted rendezvous hashing: the key moves only when its own server leaves.
fn rendezvous(servers: &[(String, u16)], key: &[u8]) -> Option<String> {
    servers
        .iter()
        .map(|(server, weight)| {
            let digest = sha256(&[key, b"\0", server.as_bytes()].concat());
            let bits = u32::from_be_bytes(digest[..4].try_into().expect("four bytes"));
            let unit = (f64::from(bits) + 1.0) / 4_294_967_296.0;
            (-unit.ln() / f64::from(*weight), server)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, server)| server.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn farm(weights: &[u16]) -> Vec<(String, u16)> {
        weights
            .iter()
            .enumerate()
            .map(|(index, weight)| (format!("s{index}"), *weight))
            .collect()
    }

    fn owners(servers: &[(String, u16)]) -> Vec<String> {
        (0..1000)
            .map(|key| rendezvous(servers, format!("key-{key}").as_bytes()).expect("server"))
            .collect()
    }

    #[test]
    fn a_key_always_maps_to_the_same_server() {
        let servers = farm(&[1, 1, 1]);
        assert_eq!(owners(&servers), owners(&servers));
    }

    #[test]
    fn removing_a_server_remaps_only_its_own_keys() {
        let full = farm(&[1, 1, 1, 1]);
        let before = owners(&full);
        let after = owners(&full[..3]);
        for (old, new) in before.iter().zip(&after) {
            assert!(old == new || old == "s3", "{old} moved to {new}");
        }
        assert!(before.iter().any(|owner| owner == "s3"));
    }

    #[test]
    fn weights_scale_the_share_of_keys() {
        let counts = owners(&farm(&[3, 1]))
            .iter()
            .fold([0_u32; 2], |mut counts, owner| {
                counts[usize::from(owner == "s1")] += 1;
                counts
            });
        assert!(
            (650..850).contains(&counts[0]),
            "heavy server got {}",
            counts[0]
        );
    }

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
