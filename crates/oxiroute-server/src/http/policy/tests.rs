use oxiroute_config::{
    HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpProxyPolicy, HttpRedirectLocation,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpSameSite,
};

use http::header::{CONNECTION, UPGRADE};

use super::*;
use crate::http::action::ProxyPolicyPlan;

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

fn apply_response_decisions(headers: &mut HeaderMap, decisions: Vec<ResponseHeaderDecision>) {
    for decision in decisions {
        match decision {
            ResponseHeaderDecision::Remove(name) => {
                headers.remove(name);
            }
            ResponseHeaderDecision::Set { name, value } => {
                headers.insert(name, value);
            }
            ResponseHeaderDecision::Add { name, value } => {
                headers.append(name, value);
            }
        }
    }
}

fn response_policy() -> ProxyPolicyPlan {
    ProxyPolicyPlan::compile(&HttpProxyPolicy {
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-order".into(),
                value: "set".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-order".into(),
                value: "add".into(),
                always: false,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-remove".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/internal".into(),
            to: "/".into(),
        }],
        response_cookie_attributes: vec![HttpCookieAttributePolicy {
            name: "sid".into(),
            secure: Some(false),
            http_only: Some(true),
            same_site: Some(HttpSameSite::Lax),
        }],
        ..HttpProxyPolicy::default()
    })
}

#[test]
fn response_decisions_order_connection_policy_and_cookie_phases() {
    let mut headers = HeaderMap::new();
    headers.append(CONNECTION, HeaderValue::from_static("x-first, keep-alive"));
    headers.append(CONNECTION, HeaderValue::from_static("x-second"));
    headers.insert("x-first", HeaderValue::from_static("remove"));
    headers.insert("x-second", HeaderValue::from_static("remove"));
    headers.insert("x-remove", HeaderValue::from_static("remove"));
    headers.append(
        SET_COOKIE,
        HeaderValue::from_static("sid=1; Path=/internal"),
    );
    let decisions =
        decide_response_headers(StatusCode::OK, &headers, &response_policy(), true).unwrap();
    let names = decisions
        .iter()
        .map(|decision| match decision {
            ResponseHeaderDecision::Remove(name) => format!("remove:{name}"),
            ResponseHeaderDecision::Set { name, .. } => format!("set:{name}"),
            ResponseHeaderDecision::Add { name, .. } => format!("add:{name}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "remove:x-first",
            "remove:x-second",
            "remove:connection",
            "set:x-order",
            "add:x-order",
            "remove:x-remove",
            "remove:set-cookie",
            "add:set-cookie",
        ]
    );
    apply_response_decisions(&mut headers, decisions);
    assert_eq!(
        headers
            .get_all("x-order")
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        [b"set".as_slice(), b"add".as_slice()]
    );
    assert_eq!(headers[SET_COOKIE], "sid=1; Path=/; HttpOnly; SameSite=Lax");
    for name in ["connection", "x-first", "x-second", "x-remove"] {
        assert!(!headers.contains_key(name), "{name}");
    }
}

#[test]
fn absent_hop_by_hop_headers_need_no_removal_decision() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("text/plain"));
    let plain = ProxyPolicyPlan::compile(&HttpProxyPolicy::default());
    assert!(
        decide_response_headers(StatusCode::OK, &headers, &plain, true)
            .unwrap()
            .is_empty()
    );

    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    let decisions = decide_response_headers(StatusCode::OK, &headers, &plain, true).unwrap();
    assert_eq!(decisions, [ResponseHeaderDecision::Remove(UPGRADE)]);
}

#[test]
fn response_decision_tables_gate_status_and_validate_all_nominations_first() {
    let policy = response_policy();
    for (status, has_add) in [(StatusCode::OK, true), (StatusCode::NOT_FOUND, false)] {
        let decisions = decide_response_headers(status, &HeaderMap::new(), &policy, false).unwrap();
        assert_eq!(
            decisions.iter().any(|decision| matches!(
                decision,
                ResponseHeaderDecision::Add { name, .. } if name == "x-order"
            )),
            has_add
        );
    }

    let mut headers = HeaderMap::new();
    headers.append(CONNECTION, HeaderValue::from_static("x-first"));
    headers.append(CONNECTION, HeaderValue::from_static("x-second, two words"));
    assert_eq!(
        decide_response_headers(StatusCode::OK, &headers, &policy, true),
        Err(ResponsePolicyError::InvalidConnectionNomination)
    );
}

#[test]
fn cookie_rewrite_table_preserves_exact_case_whitespace_and_malformed_values() {
    let policy = response_policy();
    for (input, expected) in [
        (
            "sid=1; Path=/internal; secure; HTTPONLY; SameSite=Strict; Priority=High",
            "sid=1; Path=/; HTTPONLY; SameSite=Lax; Priority=High",
        ),
        (
            "sid=1;\tPATH=/internal;Secure;httponly;samesite=None",
            "sid=1;\tPATH=/;httponly;SameSite=Lax",
        ),
        ("SID=1; Path=/internal; secure", "SID=1; Path=/; secure"),
        (
            "sid=1; Path= /internal",
            "sid=1; Path= /internal; HttpOnly; SameSite=Lax",
        ),
        ("malformed; Path=/internal", "malformed; Path=/internal"),
        ("=value; Path=/internal", "=value; Path=/internal"),
    ] {
        assert_eq!(
            rewrite_cookie(&HeaderValue::from_static(input), &policy).unwrap(),
            expected
        );
    }

    let non_utf8 = HeaderValue::from_bytes(b"sid=\xff; Path=/internal").unwrap();
    assert_eq!(rewrite_cookie(&non_utf8, &policy).unwrap(), non_utf8);
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
    let RedirectLocationPlan::RequestTemplate { segments, .. } =
        RedirectLocationPlan::compile(&template("before-$scheme://$host$request_uri-after", None))
            .expect("closed redirect template")
    else {
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

#[test]
fn address_text_matches_the_standard_formatting() {
    for text in [
        "0.0.0.0",
        "127.0.0.1",
        "192.0.2.255",
        "198.51.100.40",
        "203.0.113.99",
        "::",
        "::1",
        "2001:db8::1",
        "2001:db8:85a3:8d3:1319:8a2e:370:7348",
        "::ffff:192.0.2.1",
    ] {
        let address: IpAddr = text.parse().expect("address");
        assert_eq!(
            IpText::new(address).as_bytes(),
            address.to_string().as_bytes()
        );
    }
}

#[test]
fn forwarded_for_is_the_client_alone_or_appended_to_what_arrived() {
    let plan = request_mutation(HttpRequestHeaderValue::AppendedXForwardedFor {
        max_bytes: 64,
        except_source_cidrs: Vec::new(),
    });
    let client = Some("192.0.2.7".parse().expect("client"));
    let value = |headers: &HeaderMap| {
        let decision = decide_request_header(
            &plan.request_headers[0],
            request_context(None, client, headers),
        );
        let Ok(RequestHeaderDecision::Set { value, .. }) = decision else {
            panic!("a set decision: {decision:?}");
        };
        value.complete(None).expect("complete").expect("a value")
    };
    assert_eq!(value(&HeaderMap::new()), "192.0.2.7");
    let mut headers = HeaderMap::new();
    headers.append("x-forwarded-for", HeaderValue::from_static("198.51.100.1"));
    headers.append("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
    assert_eq!(value(&headers), "198.51.100.1, 203.0.113.9, 192.0.2.7");
}
