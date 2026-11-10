use super::*;

#[test]
fn upstream_path_rewrite_preserves_the_query() {
    let mut uri = "/api/items?id=7".parse().expect("request URI");
    rewrite_upstream_path(
        &mut uri,
        &HttpProxyPathRewrite {
            from: "/api/".into(),
            to: "/v1/".into(),
        },
    )
    .expect("rewritten request URI");
    assert_eq!(
        uri.path_and_query().map(http::uri::PathAndQuery::as_str),
        Some("/v1/items?id=7")
    );
}

#[test]
fn nginx_host_preserves_ipv6_authority_brackets() {
    let authority = "[2001:db8::1]:8080".parse::<Authority>().unwrap();
    let fallback = HeaderValue::from_static("fallback.example");

    assert_eq!(
        nginx_request_host(Some(&authority), &fallback).unwrap(),
        "[2001:db8::1]"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_downstream_preserves_existing_x_forwarded_for_without_an_ip_peer() {
    let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 128,
                except_source_cidrs: vec!["127.0.0.0/8".into()],
            },
        }],
        ..HttpProxyPolicy::default()
    });
    let selector = Arc::new(
        RoundRobinPool::new_named(
            "unix-xff".into(),
            [RuntimeEndpoint::Socket {
                address: "127.0.0.1:1".parse().unwrap(),
            }],
            UpstreamAlgorithm::RoundRobin,
            false,
        )
        .unwrap(),
    );
    let route = HttpRoutePlan {
        access: None,
        action: HttpActionPlan::Proxy(crate::http::action::ProxyActionPlan {
            pool: Arc::new(UpstreamPlan::with_policy(
                selector,
                None,
                None,
                None,
                UpstreamConnectionReuse::Safe,
            )),
            rescue_pool: None,
            select: None,
            policy: Box::new(policy),
        }),
        policy: crate::http::action::RoutePolicyPlan::compile(
            oxiroute_config::HttpRoutePolicy::default(),
        ),
        route_id: "unix-xff".into(),
    };
    let (session, _client) = unix_request_session(
        b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: trusted\r\n\r\n",
    )
    .await;

    let decisions = pingora_request_header_decisions(&session, &route, None).unwrap();
    let RequestHeaderDecision::Set { value, .. } = &decisions[0] else {
        panic!("X-Forwarded-For set decision")
    };
    assert_eq!(
        value.complete(None).unwrap(),
        Some(HeaderValue::from_static("trusted"))
    );

    let (session, _client) =
        unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
    let decisions = pingora_request_header_decisions(&session, &route, None).unwrap();
    let RequestHeaderDecision::Set { value, .. } = &decisions[0] else {
        panic!("X-Forwarded-For no-op decision")
    };
    assert_eq!(value.complete(None).unwrap(), None);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one request-value matrix keeps Pingora policy behavior comparable"
)]
async fn pingora_request_values_join_fields_and_honor_source_exceptions() {
    let request_headers = vec![
        HttpRequestHeaderMutation::Set {
            name: "x-literal".into(),
            value: HttpRequestHeaderValue::Literal {
                value: "literal".into(),
            },
        },
        HttpRequestHeaderMutation::Set {
            name: "x-authority".into(),
            value: HttpRequestHeaderValue::IncomingAuthority,
        },
        HttpRequestHeaderMutation::Set {
            name: "x-normalized".into(),
            value: HttpRequestHeaderValue::NormalizedHost,
        },
        HttpRequestHeaderMutation::Set {
            name: "x-nginx".into(),
            value: HttpRequestHeaderValue::NginxHost {
                fallback: "fallback.test".into(),
            },
        },
        HttpRequestHeaderMutation::Set {
            name: "x-client".into(),
            value: HttpRequestHeaderValue::ClientIp,
        },
        HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 128,
                except_source_cidrs: Vec::new(),
            },
        },
        HttpRequestHeaderMutation::Set {
            name: "x-scheme".into(),
            value: HttpRequestHeaderValue::DownstreamScheme,
        },
        HttpRequestHeaderMutation::Set {
            name: "x-joined".into(),
            value: HttpRequestHeaderValue::IncomingHeader {
                name: "x-source".into(),
                max_bytes: 32,
            },
        },
        HttpRequestHeaderMutation::Set {
            name: "x-selected".into(),
            value: HttpRequestHeaderValue::SelectedUpstreamHost,
        },
        HttpRequestHeaderMutation::Remove {
            name: "x-remove".into(),
        },
    ];
    let policy = HttpProxyPolicy {
        request_headers,
        ..HttpProxyPolicy::default()
    };
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            policy,
            UpstreamConnectionReuse::Safe,
            b"GET / HTTP/1.1\r\nHost: Client.Example.:8080\r\nX-Forwarded-For: trusted\r\nX-Source: one\r\nX-Source: two\r\nX-Remove: stale\r\n\r\n",
        )
        .await;
    context.authority = Some(
        "Client.Example.:8080"
            .parse()
            .expect("mixed-case authority"),
    );
    context.request_header_decisions = pingora_request_header_decisions(
        &session,
        context.route.as_ref().expect("request route"),
        context.authority.as_ref(),
    )
    .expect("request header decisions");

    let PreparedUpstreamRequest::Owned(request) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare request values")
    else {
        panic!("request policy requires owned preparation")
    };
    for (name, expected) in [
        ("x-literal", "literal"),
        ("x-authority", "Client.Example.:8080"),
        ("x-normalized", "client.example."),
        ("x-nginx", "client.example"),
        ("x-client", "127.0.0.1"),
        ("x-forwarded-for", "trusted, 127.0.0.1"),
        ("x-scheme", "http"),
        ("x-joined", "one, two"),
        ("x-selected", "example.test"),
    ] {
        assert_eq!(
            request.headers.get(name),
            Some(&HeaderValue::from_static(expected)),
            "{name}"
        );
    }
    assert!(!request.headers.contains_key("x-remove"));

    let excepted = HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-forwarded-for".into(),
            value: HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 128,
                except_source_cidrs: vec!["127.0.0.0/8".into()],
            },
        }],
        ..HttpProxyPolicy::default()
    };
    let (_proxy, session, context, _client) = request_preparation_fixture(
        excepted,
        UpstreamConnectionReuse::Safe,
        b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Forwarded-For: trusted\r\n\r\n",
    )
    .await;
    let mut request = session.req_header().clone();
    apply_request_header_mutations(&mut request, &context).expect("excepted request mutation");
    assert_eq!(request.headers["x-forwarded-for"], "trusted");
}

#[cfg(unix)]
#[tokio::test]
async fn pingora_join_bounds_include_inserted_separators() {
    let (mut session, _client) =
        unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
    let name = HeaderName::from_static("x-source");
    session
        .req_header_mut()
        .append_header(name.clone(), "abc")
        .unwrap();
    session
        .req_header_mut()
        .append_header(name.clone(), "")
        .unwrap();

    assert_eq!(
        crate::http::policy::join_header_values(&session.req_header().headers, &name, 5),
        Ok(Some(b"abc, ".to_vec()))
    );
    assert_eq!(
        crate::http::policy::join_header_values(&session.req_header().headers, &name, 4),
        Err(RequestPolicyError::SourceTooLarge)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn pingora_redirect_templates_characterize_context_and_bounds() {
    let (session, _client) =
        unix_request_session(b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n").await;
    let template = |value: &str, fallback: Option<&str>| HttpRedirectLocation::RequestTemplate {
        value: value.into(),
        nginx_host_fallback: fallback.map(str::to_owned),
    };
    let uri = "/path/to?q=1".parse().expect("redirect URI");
    for (location, authority, expected) in [
        (
            template("$scheme://$host$request_uri", None),
            Some("Normalized.Test.:8443"),
            Some("http://normalized.test/path/to?q=1"),
        ),
        (
            template("//$host$request_uri", Some("fallback.test")),
            None,
            Some("//fallback.test/path/to?q=1"),
        ),
        (template("$unknown", None), None, None),
    ] {
        let location = RedirectLocationPlan::compile(&location);
        let authority = authority.map(|value| value.parse().expect("redirect authority"));
        let actual = location
            .as_ref()
            .and_then(|location| redirect_location(location, &session, authority.as_ref(), &uri));
        assert_eq!(
            actual.as_ref().and_then(|value| value.to_str().ok()),
            expected
        );
    }

    for (length, accepted) in [(8192, true), (8193, false)] {
        let location = HttpRedirectLocation::Literal {
            value: "x".repeat(length),
        };
        let location = RedirectLocationPlan::compile(&location);
        assert_eq!(
            location
                .as_ref()
                .and_then(|location| redirect_location(location, &session, None, &uri))
                .is_some(),
            accepted,
            "literal length {length}"
        );
    }
    let boundary_uri = format!("/{}", "x".repeat(8191))
        .parse()
        .expect("boundary redirect URI");
    assert!(
        RedirectLocationPlan::compile(&template("$request_uri", None))
            .and_then(|location| { redirect_location(&location, &session, None, &boundary_uri) })
            .is_some()
    );
    let oversized_uri = format!("/{}", "x".repeat(8192))
        .parse()
        .expect("oversized redirect URI");
    assert!(
        RedirectLocationPlan::compile(&template("$request_uri", None))
            .and_then(|location| { redirect_location(&location, &session, None, &oversized_uri) })
            .is_none()
    );
}

#[tokio::test]
async fn no_case_single_host_preparation_borrows_without_cloning() {
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        HttpProxyPolicy::default(),
        UpstreamConnectionReuse::Safe,
        b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
    )
    .await;
    assert!(!session.req_header().has_case());
    let hosts = session.req_header().headers.get_all(HOST);
    assert_eq!(hosts.iter().count(), 1);
    assert_eq!(
        hosts.iter().next(),
        Some(&HeaderValue::from_static("example.test"))
    );
    assert!(has_single_canonical_host(
        session.req_header(),
        context
            .selected_upstream_host
            .as_ref()
            .expect("selected Host")
    ));
    assert!(!upstream_request_requires_mutation(&session, &context));
    let clones = Arc::new(AtomicUsize::new(0));
    session
        .req_header_mut()
        .extensions_mut()
        .insert(CloneProbe(Arc::clone(&clones)));

    for _ in 0..2 {
        let prepared = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare canonical request");
        assert!(matches!(prepared, PreparedUpstreamRequest::Borrowed));
    }

    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn downstream_http_1_0_is_normalized_to_the_canonical_http_1_1_upstream() {
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        HttpProxyPolicy::default(),
        UpstreamConnectionReuse::Safe,
        b"GET / HTTP/1.0\r\nHost: example.test\r\n\r\n",
    )
    .await;

    let PreparedUpstreamRequest::Owned(request) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare HTTP/1.0 request")
    else {
        panic!("HTTP/1.0 requires canonical upstream normalization");
    };
    assert_eq!(request.version, http::Version::HTTP_11);
}

#[tokio::test]
async fn mutating_retry_preparation_clones_once_and_starts_clean() {
    let policy = HttpProxyPolicy {
        request_headers: vec![HttpRequestHeaderMutation::Set {
            name: "x-policy".into(),
            value: HttpRequestHeaderValue::Literal {
                value: "same".into(),
            },
        }],
        ..HttpProxyPolicy::default()
    };
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        policy,
        UpstreamConnectionReuse::Safe,
        b"GET / HTTP/1.1\r\nHost: example.test\r\nX-Policy: same\r\nx-policy: stale\r\n\r\n",
    )
    .await;
    let clones = Arc::new(AtomicUsize::new(0));
    session
        .req_header_mut()
        .extensions_mut()
        .insert(CloneProbe(Arc::clone(&clones)));

    let PreparedUpstreamRequest::Owned(mut first) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare first request")
    else {
        panic!("configured Set must own the request");
    };
    assert_eq!(first.headers.get_all("x-policy").iter().count(), 1);
    first.insert_header("x-attempt", "dirty").unwrap();

    let PreparedUpstreamRequest::Owned(second) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare retry request")
    else {
        panic!("configured Set must own the retry request");
    };
    assert_eq!(second.headers.get_all("x-policy").iter().count(), 1);
    assert!(second.headers.get("x-attempt").is_none());
    assert_eq!(
        session
            .req_header()
            .headers
            .get_all("x-policy")
            .iter()
            .count(),
        2
    );
    assert_eq!(clones.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn no_case_host_preparation_uses_semantic_value_and_duplicate_count() {
    let cases: &[(&[u8], bool)] = &[
        (b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n", false),
        (b"GET / HTTP/1.1\r\nhost: example.test\r\n\r\n", false),
        (b"GET / HTTP/1.1\r\nHOST: example.test\r\n\r\n", false),
        (b"GET / HTTP/1.1\r\nHost: other.test\r\n\r\n", true),
    ];

    for (raw_request, should_own) in cases {
        let (proxy, mut session, mut context, _client) = request_preparation_fixture(
            HttpProxyPolicy::default(),
            UpstreamConnectionReuse::Safe,
            raw_request,
        )
        .await;
        let clones = Arc::new(AtomicUsize::new(0));
        session
            .req_header_mut()
            .extensions_mut()
            .insert(CloneProbe(Arc::clone(&clones)));

        let prepared = proxy
            .prepare_upstream_request(&mut session, &mut context)
            .await
            .expect("prepare Host request");
        assert_eq!(
            matches!(prepared, PreparedUpstreamRequest::Owned(_)),
            *should_own,
            "request: {}",
            String::from_utf8_lossy(raw_request)
        );
        let prepared_request = match &prepared {
            PreparedUpstreamRequest::Borrowed => session.req_header(),
            PreparedUpstreamRequest::Owned(request) => request,
        };
        let wire = pingora::protocols::http::v1::client::http_req_header_to_wire(prepared_request)
            .expect("serialize prepared request");
        let wire = String::from_utf8(wire.to_vec()).expect("ASCII request wire");
        assert_eq!(wire.matches("\r\nHost: example.test\r\n").count(), 1);
        assert!(!wire.contains("\r\nhost:"));
        assert!(!wire.contains("\r\nHOST:"));
        assert_eq!(clones.load(Ordering::Relaxed), usize::from(*should_own));
    }
}

#[tokio::test]
async fn no_case_duplicate_host_rejects_borrowing_and_preserves_field_order() {
    // Pingora 0.9 refuses duplicate Host fields while it parses an HTTP/1 head, so the second one is added here.
    let raw_request =
        b"GET / HTTP/1.1\r\nX-Before: one\r\nHost: first.test\r\nX-After: two\r\n\r\n";
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        HttpProxyPolicy::default(),
        UpstreamConnectionReuse::Safe,
        raw_request,
    )
    .await;
    session
        .req_header_mut()
        .append_header(HOST, HeaderValue::from_static("second.test"))
        .expect("second Host");
    assert!(!session.req_header().has_case());
    assert_eq!(
        session
            .req_header()
            .headers
            .get_all(HOST)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        [b"first.test".as_slice(), b"second.test".as_slice()]
    );

    let PreparedUpstreamRequest::Owned(prepared) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare duplicate Host request")
    else {
        panic!("duplicate Host fields must reject borrowed preparation");
    };
    let wire = pingora::protocols::http::v1::client::http_req_header_to_wire(&prepared)
        .expect("serialize duplicate Host preparation");
    assert_eq!(
        wire,
        &b"GET / HTTP/1.1\r\nx-before: one\r\nHost: example.test\r\nx-after: two\r\n\r\n"[..]
    );
    assert_eq!(session.req_header().headers.get_all(HOST).iter().count(), 2);
}

#[test]
fn case_preserving_host_still_requires_canonical_wire_spelling() {
    let selected = HeaderValue::from_static("example.test");
    for (name, expected) in [("Host", true), ("host", false), ("HOST", false)] {
        let mut request = pingora::http::RequestHeader::build("GET", b"/", None)
            .expect("case-preserving request");
        request
            .append_header(name.to_owned(), selected.clone())
            .expect("Host header");
        assert!(request.has_case());
        assert_eq!(has_single_canonical_host(&request, &selected), expected);
    }
}

#[tokio::test]
async fn connection_close_policy_promotes_once() {
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        HttpProxyPolicy::default(),
        UpstreamConnectionReuse::Never,
        b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
    )
    .await;
    let clones = Arc::new(AtomicUsize::new(0));
    session
        .req_header_mut()
        .extensions_mut()
        .insert(CloneProbe(Arc::clone(&clones)));

    let PreparedUpstreamRequest::Owned(prepared) = proxy
        .prepare_upstream_request(&mut session, &mut context)
        .await
        .expect("prepare close-policy request")
    else {
        panic!("Connection close policy must own the request");
    };

    assert_eq!(prepared.headers.get(CONNECTION).unwrap(), "close");
    assert_eq!(clones.load(Ordering::Relaxed), 1);
}

#[test]
fn a_client_close_is_not_forwarded_but_other_connection_options_are() {
    let mut request = pingora::http::RequestHeader::build("GET", b"/", None).expect("request");
    request
        .append_header("Connection", "Upgrade, Close")
        .expect("connection");
    request
        .append_header("connection", "keep-alive")
        .expect("connection");
    assert!(has_downstream_close(&request.headers));
    remove_downstream_close(&mut request).expect("close removed");
    assert!(!has_downstream_close(&request.headers));
    assert_eq!(
        request.headers.get(CONNECTION).expect("kept"),
        "Upgrade, keep-alive"
    );
}

#[test]
fn a_request_without_a_client_close_keeps_its_connection_header_untouched() {
    let mut request = pingora::http::RequestHeader::build("GET", b"/", None).expect("request");
    request
        .append_header("Connection", "keep-alive")
        .expect("connection");
    assert!(!has_downstream_close(&request.headers));
    remove_downstream_close(&mut request).expect("untouched");
    assert_eq!(request.headers.get(CONNECTION).expect("kept"), "keep-alive");
    let mut bare = pingora::http::RequestHeader::build("GET", b"/", None).expect("request");
    remove_downstream_close(&mut bare).expect("untouched");
    assert!(bare.headers.get(CONNECTION).is_none());
}

#[test]
fn a_preserved_host_is_the_host_header_of_an_origin_form_request_and_the_authority_otherwise() {
    let endpoint = RuntimeEndpoint::Socket {
        address: "127.0.0.1:9".parse().expect("address"),
    };
    let policy = HttpUpstreamHost::PreserveIncoming;
    let mut origin_form = pingora::http::RequestHeader::build("GET", b"/x", None).expect("request");
    origin_form
        .insert_header("host", "Example.test:8080")
        .expect("host");
    let authority = request_authority(&origin_form).expect("authority");
    let host = selected_upstream_host_for(&origin_form, &endpoint, &policy, authority.as_ref())
        .expect("host");
    assert_eq!(host, Some(HeaderValue::from_static("Example.test:8080")));

    let mut absolute = pingora::http::RequestHeader::build("GET", b"/x", None).expect("request");
    absolute.set_uri("http://other.test/x".parse().expect("uri"));
    let authority = request_authority(&absolute).expect("authority");
    let host = selected_upstream_host_for(&absolute, &endpoint, &policy, authority.as_ref())
        .expect("host");
    assert_eq!(host, Some(HeaderValue::from_static("other.test")));

    let none = pingora::http::RequestHeader::build("GET", b"/x", None).expect("request");
    assert_eq!(
        selected_upstream_host_for(&none, &endpoint, &policy, None).expect("host"),
        None
    );
}
