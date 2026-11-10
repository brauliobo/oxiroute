use super::*;

#[test]
fn nginx_gzip_status_eligibility_is_exact() {
    let gzip = HttpGzipPlan {
        level: 1,
        content_types: Box::from(["text/html".into()]),
        min_length_bytes: 20,
        min_http_version: HttpGzipMinimumVersion::Http11,
        disable_on_via: true,
        vary: true,
    };
    for (status, expected) in [
        (http::StatusCode::OK, true),
        (http::StatusCode::FORBIDDEN, true),
        (http::StatusCode::NOT_FOUND, true),
        (http::StatusCode::CREATED, false),
        (http::StatusCode::MOVED_PERMANENTLY, false),
        (http::StatusCode::INTERNAL_SERVER_ERROR, false),
        (http::StatusCode::PARTIAL_CONTENT, false),
    ] {
        let mut response = pingora::http::ResponseHeader::build(status, Some(1)).unwrap();
        response.insert_header(CONTENT_TYPE, "text/html").unwrap();
        assert_eq!(gzip_matches(&gzip, &response), expected, "{status}");
    }
    let mut encoded = pingora::http::ResponseHeader::build(http::StatusCode::OK, Some(2)).unwrap();
    encoded.insert_header(CONTENT_TYPE, "text/html").unwrap();
    encoded.insert_header(CONTENT_ENCODING, "br").unwrap();
    assert!(!gzip_matches(&gzip, &encoded));
}

#[test]
fn http_terminal_results_use_bounded_categories() {
    use crate::HttpOperationResult;

    assert_eq!(
        classify_http_result(None, Some(204)),
        HttpOperationResult::Success
    );
    assert_eq!(
        classify_http_result(None, Some(404)),
        HttpOperationResult::ClientError
    );
    assert_eq!(
        classify_http_result(None, Some(503)),
        HttpOperationResult::ServerError
    );

    let upstream = Error::new_up(ErrorType::ConnectRefused);
    assert_eq!(
        classify_http_result(Some(&upstream), None),
        HttpOperationResult::UpstreamError
    );
    let timeout = Error::new_up(ErrorType::ReadTimedout);
    assert_eq!(
        classify_http_result(Some(&timeout), None),
        HttpOperationResult::Timeout
    );
}

fn response_characterization_policy() -> ProxyPolicyPlan {
    ProxyPolicyPlan::compile(&HttpProxyPolicy {
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-set".into(),
                value: "set".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-add".into(),
                value: "added".into(),
                always: false,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-always".into(),
                value: "always".into(),
                always: true,
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

fn empty_proxy_policy() -> ProxyPolicyPlan {
    ProxyPolicyPlan::compile(&HttpProxyPolicy::default())
}

fn response_headers_fixture() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append("x-set", HeaderValue::from_static("old-one"));
    headers.append("x-set", HeaderValue::from_static("old-two"));
    headers.append("x-add", HeaderValue::from_static("upstream"));
    headers.insert("x-remove", HeaderValue::from_static("remove"));
    headers.append(
        SET_COOKIE,
        HeaderValue::from_static(
            "sid=1; Path=/internal; secure; HTTPONLY; SameSite=Strict; Priority=High",
        ),
    );
    headers.append(
        SET_COOKIE,
        HeaderValue::from_static("sid=2; Path=/internal"),
    );
    headers.append(
        SET_COOKIE,
        HeaderValue::from_static("other=3; Path=/internal"),
    );
    headers
}

fn values(headers: &HeaderMap, name: impl http::header::AsHeaderName) -> Vec<&[u8]> {
    headers
        .get_all(name)
        .iter()
        .map(HeaderValue::as_bytes)
        .collect()
}

#[test]
fn response_policy_adapters_match_mutation_status_and_cookie_ordering() {
    let policy = response_characterization_policy();
    for (status, expected_adds) in [(StatusCode::OK, 2), (StatusCode::NOT_FOUND, 1)] {
        let initial = response_headers_fixture();
        let mut map = initial.clone();
        apply_response_policy_map(status, &mut map, &policy).expect("HeaderMap policy");

        let mut response = pingora::http::ResponseHeader::build(status, None).unwrap();
        response.headers = initial;
        apply_response_policy(&mut response, &policy, true).expect("ResponseHeader policy");

        assert_eq!(response.headers, map);
        assert_eq!(values(&map, "x-set"), [b"set".as_slice()]);
        assert_eq!(values(&map, "x-add").len(), expected_adds);
        assert_eq!(values(&map, "x-always"), [b"always".as_slice()]);
        assert!(!map.contains_key("x-remove"));
        assert_eq!(
            values(&map, SET_COOKIE),
            [
                b"sid=1; Path=/; HTTPONLY; SameSite=Lax; Priority=High".as_slice(),
                b"sid=2; Path=/; HttpOnly; SameSite=Lax".as_slice(),
                b"other=3; Path=/".as_slice(),
            ]
        );
    }
}

#[test]
fn response_policy_adapters_preserve_non_utf8_cookies() {
    let policy = response_characterization_policy();
    let cookie = HeaderValue::from_bytes(b"sid=\xff; Path=/internal").unwrap();
    let mut map = HeaderMap::new();
    map.append(SET_COOKIE, cookie.clone());
    apply_response_policy_map(StatusCode::OK, &mut map, &policy).unwrap();
    assert_eq!(map[SET_COOKIE], cookie);

    let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
    response.append_header(SET_COOKIE, cookie.clone()).unwrap();
    apply_response_policy(&mut response, &policy, true).unwrap();
    assert_eq!(response.headers[SET_COOKIE], cookie);
}

fn hop_by_hop_fixture() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(CONNECTION, HeaderValue::from_static("x-first, keep-alive"));
    headers.append(CONNECTION, HeaderValue::from_static("x-second"));
    headers.insert("x-first", HeaderValue::from_static("remove"));
    headers.insert("x-second", HeaderValue::from_static("remove"));
    headers.insert(TE, HeaderValue::from_static("trailers"));
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert("x-end-to-end", HeaderValue::from_static("keep"));
    headers
}

#[test]
fn hop_by_hop_adapters_remove_nominations_from_every_connection_field() {
    let initial = hop_by_hop_fixture();
    let mut map = initial.clone();
    apply_response_policy_map(StatusCode::OK, &mut map, &empty_proxy_policy()).unwrap();

    let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
    response.headers = initial;
    apply_response_policy(&mut response, &empty_proxy_policy(), true).unwrap();

    assert_eq!(response.headers, map);
    for name in [
        "connection",
        "x-first",
        "x-second",
        "keep-alive",
        "te",
        "upgrade",
    ] {
        assert!(!map.contains_key(name), "{name}");
    }
    assert_eq!(map["x-end-to-end"], "keep");
}

#[test]
fn hop_by_hop_adapters_reject_invalid_nominations_before_mutating() {
    let mut initial = HeaderMap::new();
    initial.insert(CONNECTION, HeaderValue::from_static("x-first, two words"));
    initial.insert("x-first", HeaderValue::from_static("still-present"));
    let mut map = initial.clone();
    assert!(apply_response_policy_map(StatusCode::OK, &mut map, &empty_proxy_policy()).is_err());
    assert_eq!(map, initial);

    let mut response = pingora::http::ResponseHeader::build(200, None).unwrap();
    response.headers = initial.clone();
    assert!(apply_response_policy(&mut response, &empty_proxy_policy(), true).is_err());
    assert_eq!(response.headers, initial);
}

#[test]
fn proxy_add_headers_append_and_honor_nginx_statuses_and_always() {
    let policy = HttpProxyPolicy {
        response_headers: vec![
            HttpResponseHeaderMutation::Add {
                name: "x-selected".into(),
                value: "new".into(),
                always: false,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-always".into(),
                value: "new".into(),
                always: true,
            },
        ],
        ..HttpProxyPolicy::default()
    };
    let policy = ProxyPolicyPlan::compile(&policy);
    let mut error = pingora::http::ResponseHeader::build(404, None).expect("response");
    error
        .append_header("x-selected", "upstream")
        .expect("upstream header");

    apply_response_policy(&mut error, &policy, true).expect("error response policy");

    assert_eq!(error.headers.get_all("x-selected").iter().count(), 1);
    assert_eq!(error.headers.get("x-always").unwrap(), "new");

    let mut success = pingora::http::ResponseHeader::build(200, None).expect("response");
    success
        .append_header("x-selected", "upstream")
        .expect("upstream header");
    apply_response_policy(&mut success, &policy, true).expect("success response policy");
    assert_eq!(success.headers.get_all("x-selected").iter().count(), 2);
}

#[test]
fn reverse_proxy_does_not_install_disabled_compression() {
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("http", "http", "127.0.0.1:8080", 100)
        .expect("listener metrics");
    let service = Arc::new(HttpServicePlan::new(
        Some(1024),
        HashMap::new(),
        Duration::from_secs(1),
        RouteTable::default(),
    ));
    let proxy = HttpReverseProxy::new(service, metrics);
    let mut modules = HttpModules::new();

    proxy.init_downstream_modules(&mut modules);

    assert!(
        modules
            .build_ctx()
            .get::<pingora::modules::http::compression::ResponseCompression>()
            .is_none()
    );
}

#[tokio::test]
async fn grpc_trailer_hooks_leave_trailers_and_body_untouched() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("trailer listener");
    let client = TcpStream::connect(listener.local_addr().expect("trailer address"));
    let accept = listener.accept();
    let (client, downstream) = tokio::join!(client, accept);
    let _client = client.expect("trailer client");
    let (downstream, _) = downstream.expect("trailer connection");
    let downstream: Stream = Box::new(pingora::protocols::l4::stream::Stream::from(downstream));
    let mut session = Session::new_h1(downstream);
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("grpc", "http", "127.0.0.1:50051", 10)
        .expect("gRPC listener metrics");
    let service = Arc::new(HttpServicePlan::new(
        Some(1024),
        HashMap::new(),
        Duration::from_secs(1),
        RouteTable::default(),
    ));
    let proxy = HttpReverseProxy::new(service, metrics);
    let mut context = proxy.new_ctx();
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
    trailers.insert("grpc-message", http::HeaderValue::from_static("completed"));
    let expected = trailers.clone();

    proxy
        .upstream_response_trailer_filter(&mut session, &mut trailers, &mut context)
        .expect("upstream trailer hook");
    let body = proxy
        .response_trailer_filter(&mut session, &mut trailers, &mut context)
        .await
        .expect("downstream trailer hook");

    assert_eq!(trailers, expected);
    assert_eq!(body, None);
}

#[test]
fn failure_reasons_are_clean_text_the_access_log_writes_unchecked() {
    let reasons = [
        ErrorReason::Connect,
        ErrorReason::UpstreamHeader,
        ErrorReason::UpstreamIdle,
        ErrorReason::UpstreamWrite,
        ErrorReason::TotalDeadline,
        ErrorReason::Evacuated,
    ]
    .map(ErrorReason::as_str)
    .into_iter()
    .chain([
        "retry_budget_spent",
        "pool_unavailable",
        "client_closed",
        "queue_timeout",
        "connect_failed",
        "upstream_reset",
        "protocol_error",
    ]);
    for reason in reasons {
        assert_eq!(crate::observability::logging::redact_text(reason), reason);
    }
}
