use std::net::IpAddr;

use http::{HeaderValue, uri::Authority};

use crate::http_action::{
    MAX_REDIRECT_LOCATION_BYTES, RedirectLocationPlan, RedirectTemplateSegment,
};

#[derive(Clone, Copy)]
pub(crate) struct RedirectContext<'a> {
    pub(crate) scheme: &'a str,
    pub(crate) normalized_host: Option<&'a str>,
    pub(crate) request_uri: &'a str,
}

pub(crate) fn expand_redirect_location(
    location: &RedirectLocationPlan,
    context: RedirectContext<'_>,
) -> Option<HeaderValue> {
    let RedirectLocationPlan::RequestTemplate {
        segments,
        nginx_host_fallback,
    } = location
    else {
        let RedirectLocationPlan::Literal(value) = location else {
            unreachable!()
        };
        return Some(value.clone());
    };

    let host = context
        .normalized_host
        .or(nginx_host_fallback.as_deref())
        .unwrap_or_default();
    let mut expanded = String::new();
    for segment in segments {
        expanded.push_str(match segment {
            RedirectTemplateSegment::Literal(value) => value,
            RedirectTemplateSegment::Scheme => context.scheme,
            RedirectTemplateSegment::Host => host,
            RedirectTemplateSegment::RequestUri => context.request_uri,
        });
    }
    (expanded.len() <= MAX_REDIRECT_LOCATION_BYTES)
        .then(|| HeaderValue::from_str(&expanded).ok())
        .flatten()
}

pub(crate) fn normalized_redirect_host(authority: &Authority) -> String {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    )
}

#[cfg(test)]
mod tests {
    use oxiroute_config::HttpRedirectLocation;

    use super::*;

    fn template(value: &str, fallback: Option<&str>) -> HttpRedirectLocation {
        HttpRedirectLocation::RequestTemplate {
            value: value.into(),
            nginx_host_fallback: fallback.map(Into::into),
        }
    }

    fn expand(
        location: &HttpRedirectLocation,
        scheme: &str,
        host: Option<&str>,
        request_uri: &str,
    ) -> Option<HeaderValue> {
        let location = RedirectLocationPlan::compile(location)?;
        expand_redirect_location(
            &location,
            RedirectContext {
                scheme,
                normalized_host: host,
                request_uri,
            },
        )
    }

    #[test]
    fn compiles_closed_redirect_template_segments() {
        let RedirectLocationPlan::RequestTemplate { segments, .. } = RedirectLocationPlan::compile(
            &template("before-$scheme://$host$request_uri-after", None),
        )
        .expect("closed redirect template") else {
            panic!("request template plan")
        };
        assert_eq!(
            segments.as_ref(),
            [
                RedirectTemplateSegment::Literal("before-".into()),
                RedirectTemplateSegment::Scheme,
                RedirectTemplateSegment::Literal("://".into()),
                RedirectTemplateSegment::Host,
                RedirectTemplateSegment::RequestUri,
                RedirectTemplateSegment::Literal("-after".into()),
            ]
        );

        for invalid in ["$unknown", "$", "$scheme$"] {
            assert!(
                RedirectLocationPlan::compile(&template(invalid, None)).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn expands_redirect_context_with_missing_host_fallback() {
        let location = template("$scheme://$host$request_uri", Some("fallback.test"));
        assert_eq!(
            expand(&location, "http", Some("request.test"), "/path?q=1"),
            Some(HeaderValue::from_static("http://request.test/path?q=1"))
        );
        assert_eq!(
            expand(&location, "https", None, "/path?q=1"),
            Some(HeaderValue::from_static("https://fallback.test/path?q=1"))
        );
        assert_eq!(
            expand(&template("//$host/", None), "http", None, "/"),
            Some(HeaderValue::from_static("///"))
        );
    }

    #[test]
    fn normalizes_redirect_hosts_without_authority_ports() {
        for (authority, expected) in [
            ("Actions.Example.:8443", "actions.example"),
            ("192.0.2.1:8080", "192.0.2.1"),
            ("[2001:0db8::1]:8443", "[2001:db8::1]"),
        ] {
            assert_eq!(
                normalized_redirect_host(&authority.parse().expect("HTTP authority")),
                expected,
                "{authority}"
            );
        }
    }

    #[test]
    fn enforces_exact_redirect_length_and_header_validity_bounds() {
        let literal = HttpRedirectLocation::Literal {
            value: "$scheme://$host$request_uri".into(),
        };
        assert_eq!(
            expand(&literal, "https", Some("request.test"), "/path"),
            Some(HeaderValue::from_static("$scheme://$host$request_uri"))
        );

        for (length, accepted) in [(MAX_REDIRECT_LOCATION_BYTES, true), (8193, false)] {
            let literal = HttpRedirectLocation::Literal {
                value: "x".repeat(length),
            };
            assert_eq!(
                expand(&literal, "http", None, "/").is_some(),
                accepted,
                "literal length {length}"
            );
            assert_eq!(
                expand(
                    &template("$request_uri", None),
                    "http",
                    None,
                    &"x".repeat(length),
                )
                .is_some(),
                accepted,
                "expanded length {length}"
            );
        }

        assert!(
            expand(
                &template("$request_uri", None),
                "http",
                None,
                "invalid\nlocation",
            )
            .is_none()
        );
    }
}
