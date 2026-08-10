use std::net::IpAddr;

use http::{HeaderMap, HeaderName, HeaderValue, uri::Authority};

use crate::http_action::{
    MAX_REDIRECT_LOCATION_BYTES, RedirectLocationPlan, RedirectTemplateSegment,
    RequestHeaderMutationPlan, RequestHeaderValuePlan,
};

#[derive(Clone, Copy)]
pub(crate) struct RequestPolicyContext<'a> {
    pub(crate) authority: Option<&'a Authority>,
    pub(crate) downstream_scheme: &'static str,
    pub(crate) client_ip: Option<IpAddr>,
    pub(crate) incoming_headers: &'a HeaderMap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequestHeaderDecision {
    Remove(HeaderName),
    Set {
        name: HeaderName,
        value: RequestHeaderValueDecision,
    },
}

impl RequestHeaderDecision {
    pub(crate) fn requires_mutation(&self) -> bool {
        match self {
            Self::Remove(_) => true,
            Self::Set { value, .. } => value.requires_mutation(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequestHeaderValueDecision {
    Value(HeaderValue),
    ClientIp(Option<HeaderValue>),
    XForwardedFor(XForwardedForDecision),
    SelectedUpstreamHost,
}

impl RequestHeaderValueDecision {
    fn requires_mutation(&self) -> bool {
        !matches!(
            self,
            Self::XForwardedFor(XForwardedForDecision::Preserve | XForwardedForDecision::NoOp)
        )
    }

    pub(crate) fn complete(
        &self,
        selected_upstream_host: Option<&HeaderValue>,
    ) -> Result<Option<HeaderValue>, RequestPolicyError> {
        match self {
            Self::Value(value) | Self::XForwardedFor(XForwardedForDecision::Set(value)) => {
                Ok(Some(value.clone()))
            }
            Self::ClientIp(value) => value
                .clone()
                .map(Some)
                .ok_or(RequestPolicyError::ClientIpUnavailable),
            Self::XForwardedFor(XForwardedForDecision::Preserve | XForwardedForDecision::NoOp) => {
                Ok(None)
            }
            Self::SelectedUpstreamHost => selected_upstream_host
                .cloned()
                .map(Some)
                .ok_or(RequestPolicyError::SelectedUpstreamHostUnavailable),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum XForwardedForDecision {
    Preserve,
    NoOp,
    Set(HeaderValue),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestPolicyError {
    SourceTooLarge,
    InvalidHeader,
    ClientIpUnavailable,
    SelectedUpstreamHostUnavailable,
}

pub(crate) fn decide_request_header(
    mutation: &RequestHeaderMutationPlan,
    context: RequestPolicyContext<'_>,
) -> Result<RequestHeaderDecision, RequestPolicyError> {
    let (name, value) = match mutation {
        RequestHeaderMutationPlan::Set { name, value } => (name, value),
        RequestHeaderMutationPlan::Remove { name } => {
            return Ok(RequestHeaderDecision::Remove(name.clone()));
        }
    };
    let value = match value {
        RequestHeaderValuePlan::Literal(value) => RequestHeaderValueDecision::Value(value.clone()),
        RequestHeaderValuePlan::IncomingAuthority => RequestHeaderValueDecision::Value(
            header_value(context.authority.map(Authority::as_str).unwrap_or_default())?,
        ),
        RequestHeaderValuePlan::NormalizedHost => {
            let host = context
                .authority
                .map(normalized_request_host)
                .unwrap_or_default();
            RequestHeaderValueDecision::Value(header_value(&host)?)
        }
        RequestHeaderValuePlan::NginxHost { fallback } => {
            RequestHeaderValueDecision::Value(nginx_request_host(context.authority, fallback)?)
        }
        RequestHeaderValuePlan::ClientIp => RequestHeaderValueDecision::ClientIp(
            context
                .client_ip
                .map(|client_ip| header_value(&client_ip.to_string()))
                .transpose()?,
        ),
        RequestHeaderValuePlan::AppendedXForwardedFor {
            max_bytes,
            except_source_cidrs,
        } => RequestHeaderValueDecision::XForwardedFor(decide_x_forwarded_for(
            context,
            *max_bytes,
            except_source_cidrs,
        )?),
        RequestHeaderValuePlan::DownstreamScheme => {
            RequestHeaderValueDecision::Value(HeaderValue::from_static(context.downstream_scheme))
        }
        RequestHeaderValuePlan::IncomingHeader { name, max_bytes } => {
            let value =
                join_header_values(context.incoming_headers, name, *max_bytes)?.unwrap_or_default();
            RequestHeaderValueDecision::Value(bounded_header_value(&value, *max_bytes)?)
        }
        RequestHeaderValuePlan::SelectedUpstreamHost => {
            RequestHeaderValueDecision::SelectedUpstreamHost
        }
    };
    Ok(RequestHeaderDecision::Set {
        name: name.clone(),
        value,
    })
}

fn decide_x_forwarded_for(
    context: RequestPolicyContext<'_>,
    max_bytes: usize,
    exceptions: &[crate::http_action::SourceCidr],
) -> Result<XForwardedForDecision, RequestPolicyError> {
    if context.client_ip.is_some_and(|client_ip| {
        exceptions
            .iter()
            .any(|exception| exception.contains(client_ip))
    }) {
        return Ok(XForwardedForDecision::Preserve);
    }
    let name = HeaderName::from_static("x-forwarded-for");
    let existing = join_header_values(context.incoming_headers, &name, max_bytes)?;
    let Some(client_ip) = context.client_ip else {
        return existing.map_or(Ok(XForwardedForDecision::NoOp), |value| {
            bounded_header_value(&value, max_bytes).map(XForwardedForDecision::Set)
        });
    };
    let client_ip = client_ip.to_string();
    let mut value = existing.unwrap_or_default();
    if !value.is_empty() {
        extend_bounded(&mut value, b", ", max_bytes)?;
    }
    extend_bounded(&mut value, client_ip.as_bytes(), max_bytes)?;
    bounded_header_value(&value, max_bytes).map(XForwardedForDecision::Set)
}

pub(crate) fn join_header_values(
    headers: &HeaderMap,
    name: &HeaderName,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, RequestPolicyError> {
    let mut joined = Vec::new();
    for value in headers.get_all(name) {
        if !joined.is_empty() {
            extend_bounded(&mut joined, b", ", max_bytes)?;
        }
        extend_bounded(&mut joined, value.as_bytes(), max_bytes)?;
    }
    Ok((!joined.is_empty()).then_some(joined))
}

fn extend_bounded(
    output: &mut Vec<u8>,
    value: &[u8],
    max_bytes: usize,
) -> Result<(), RequestPolicyError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > max_bytes)
    {
        return Err(RequestPolicyError::SourceTooLarge);
    }
    output.extend_from_slice(value);
    Ok(())
}

fn bounded_header_value(value: &[u8], max_bytes: usize) -> Result<HeaderValue, RequestPolicyError> {
    if value.len() > max_bytes {
        return Err(RequestPolicyError::SourceTooLarge);
    }
    HeaderValue::from_bytes(value).map_err(|_| RequestPolicyError::InvalidHeader)
}

fn header_value(value: &str) -> Result<HeaderValue, RequestPolicyError> {
    HeaderValue::from_str(value).map_err(|_| RequestPolicyError::InvalidHeader)
}

pub(crate) fn normalized_request_host(authority: &Authority) -> String {
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .map_or_else(|_| host.to_ascii_lowercase(), |ip| ip.to_string())
}

pub(crate) fn nginx_request_host(
    authority: Option<&Authority>,
    fallback: &HeaderValue,
) -> Result<HeaderValue, RequestPolicyError> {
    let Some(authority) = authority else {
        return Ok(fallback.clone());
    };
    let host = authority.host();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let normalized = unbracketed.parse::<IpAddr>().map_or_else(
        |_| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase(),
        |ip| match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    );
    header_value(&normalized)
}

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
    use oxiroute_config::{
        HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    };

    use super::*;
    use crate::http_action::ProxyPolicyPlan;

    fn request_mutation(value: HttpRequestHeaderValue) -> ProxyPolicyPlan {
        ProxyPolicyPlan::compile(&HttpProxyPolicy {
            request_headers: vec![HttpRequestHeaderMutation::Set {
                name: "x-result".into(),
                value,
            }],
            ..HttpProxyPolicy::default()
        })
    }

    fn request_context<'a>(
        authority: Option<&'a Authority>,
        client_ip: Option<IpAddr>,
        headers: &'a HeaderMap,
    ) -> RequestPolicyContext<'a> {
        RequestPolicyContext {
            authority,
            downstream_scheme: "https",
            client_ip,
            incoming_headers: headers,
        }
    }

    fn value_decision(
        policy: &ProxyPolicyPlan,
        context: RequestPolicyContext<'_>,
    ) -> RequestHeaderValueDecision {
        let RequestHeaderDecision::Set { value, .. } =
            decide_request_header(&policy.request_headers[0], context).expect("request decision")
        else {
            panic!("set decision")
        };
        value
    }

    #[test]
    fn decides_concrete_request_context_values_and_deferred_upstream_host() {
        let authority: Authority = "Client.Example.:8443".parse().unwrap();
        let headers = HeaderMap::new();
        let cases = [
            (
                HttpRequestHeaderValue::IncomingAuthority,
                RequestHeaderValueDecision::Value(HeaderValue::from_static("Client.Example.:8443")),
            ),
            (
                HttpRequestHeaderValue::NormalizedHost,
                RequestHeaderValueDecision::Value(HeaderValue::from_static("client.example.")),
            ),
            (
                HttpRequestHeaderValue::NginxHost {
                    fallback: "fallback.test".into(),
                },
                RequestHeaderValueDecision::Value(HeaderValue::from_static("client.example")),
            ),
            (
                HttpRequestHeaderValue::ClientIp,
                RequestHeaderValueDecision::ClientIp(Some(HeaderValue::from_static("192.0.2.7"))),
            ),
            (
                HttpRequestHeaderValue::DownstreamScheme,
                RequestHeaderValueDecision::Value(HeaderValue::from_static("https")),
            ),
            (
                HttpRequestHeaderValue::SelectedUpstreamHost,
                RequestHeaderValueDecision::SelectedUpstreamHost,
            ),
        ];
        for (source, expected) in cases {
            let policy = request_mutation(source);
            let decision = value_decision(
                &policy,
                request_context(
                    Some(&authority),
                    Some("192.0.2.7".parse().unwrap()),
                    &headers,
                ),
            );
            assert_eq!(decision, expected);
        }

        let policy = request_mutation(HttpRequestHeaderValue::SelectedUpstreamHost);
        let decision = value_decision(&policy, request_context(Some(&authority), None, &headers));
        assert_eq!(
            decision.complete(None),
            Err(RequestPolicyError::SelectedUpstreamHostUnavailable)
        );
        assert_eq!(
            decision.complete(Some(&HeaderValue::from_static("selected.test"))),
            Ok(Some(HeaderValue::from_static("selected.test")))
        );

        let policy = request_mutation(HttpRequestHeaderValue::ClientIp);
        let decision = value_decision(&policy, request_context(Some(&authority), None, &headers));
        assert_eq!(decision, RequestHeaderValueDecision::ClientIp(None));
        assert_eq!(
            decision.complete(None),
            Err(RequestPolicyError::ClientIpUnavailable)
        );
    }

    #[test]
    fn decides_x_forwarded_for_preserve_noop_and_set() {
        let policy = request_mutation(HttpRequestHeaderValue::AppendedXForwardedFor {
            max_bytes: 32,
            except_source_cidrs: vec!["192.0.2.0/24".into()],
        });
        let client_ip = Some("192.0.2.7".parse().unwrap());
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", HeaderValue::from_static("trusted"));
        let cases = [
            (client_ip, &headers, XForwardedForDecision::Preserve),
            (None, &HeaderMap::new(), XForwardedForDecision::NoOp),
            (
                Some("198.51.100.8".parse().unwrap()),
                &headers,
                XForwardedForDecision::Set(HeaderValue::from_static("trusted, 198.51.100.8")),
            ),
        ];
        for (client_ip, headers, expected) in cases {
            assert_eq!(
                value_decision(&policy, request_context(None, client_ip, headers)),
                RequestHeaderValueDecision::XForwardedFor(expected)
            );
        }
    }

    #[test]
    fn joins_duplicate_values_with_checked_separator_bounds() {
        let name = HeaderName::from_static("x-source");
        let mut headers = HeaderMap::new();
        headers.append(&name, HeaderValue::from_static("abc"));
        headers.append(&name, HeaderValue::from_static(""));
        assert_eq!(
            join_header_values(&headers, &name, 5),
            Ok(Some(b"abc, ".to_vec()))
        );
        assert_eq!(
            join_header_values(&headers, &name, 4),
            Err(RequestPolicyError::SourceTooLarge)
        );
    }

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
