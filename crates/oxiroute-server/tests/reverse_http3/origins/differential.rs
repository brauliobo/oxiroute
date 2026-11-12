//! One service on an https listener and an http3 listener: the same requests get the same answers from the same
//! HTTP/1.1 origin.
#[allow(clippy::wildcard_imports)]
use super::*;
use std::fmt::Write as _;
use tokio::io::AsyncReadExt as _;

type Table = [(
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [u8],
)];

const TABLE: &Table = &[
    ("GET", "/plain", &[], b""),
    ("GET", "/plain?a=1&b=%20two", &[("x-custom", "kept")], b""),
    (
        "GET",
        "/api/items?q=1",
        &[("x-secret", "s3"), ("x-forwarded-for", "192.0.2.7")],
        b"",
    ),
    (
        "POST",
        "/api/items",
        &[("content-type", "text/plain")],
        b"posted body",
    ),
    (
        "POST",
        "/plain",
        &[("content-type", "application/octet-stream")],
        &[7; 4096],
    ),
    ("HEAD", "/plain", &[], b""),
    ("GET", "/empty", &[], b""),
    ("GET", "/not-modified", &[], b""),
    ("GET", "/chunked", &[], b""),
    ("GET", "/missing", &[], b""),
    ("GET", "/big", &[], b""),
    ("GET", "/resp/policy", &[], b""),
    ("DELETE", "/plain", &[], b""),
    ("GET", "/set-cookie", &[], b""),
];

fn echo(request: &Seen) -> Reply {
    let header = |name: &str| request.header(name).unwrap_or("-").to_owned();
    let echoed = format!(
        "{} {} host={} xff={} proto={} real={} length={} custom={} secret={} body={}",
        request.method,
        request.target,
        header("host"),
        header("x-forwarded-for"),
        header("x-forwarded-proto"),
        header("x-real-ip"),
        header("content-length"),
        header("x-custom"),
        header("x-secret"),
        String::from_utf8_lossy(&request.body).len(),
    );
    let path = request.target.split('?').next().unwrap_or_default();
    match path {
        "/empty" => Reply::Send(response("204 No Content", &[("X-Fixed", "1")], b"")),
        "/not-modified" => Reply::Send(response("304 Not Modified", &[("ETag", "\"v1\"")], b"")),
        "/chunked" => Reply::Send(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Fixed: 1\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_vec(),
        ),
        "/missing" => Reply::Send(response("404 Not Found", &[("X-Fixed", "1")], b"nope")),
        "/big" => Reply::Send(response("200 OK", &[("X-Fixed", "1")], &vec![b'z'; 200_000])),
        "/set-cookie" => Reply::Send(response("200 OK", &[("Set-Cookie", "a=1"), ("Set-Cookie", "b=2; Secure")], b"cookies")),
        "/resp/policy" => Reply::Send(response("200 OK", &[("X-Origin", "internal"), ("Keep-Alive", "timeout=5")], b"policy")),
        _ if request.method == "HEAD" => Reply::Send(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Fixed: 1\r\n\r\n", echoed.len()).into_bytes(),
        ),
        _ => Reply::Send(response("200 OK", &[("X-Fixed", "1"), ("Content-Type", "text/plain")], echoed.as_bytes())),
    }
}

/// An HTTP/1.1 exchange over TLS on a connection of its own.
async fn classic(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let mut stream = support::tls_connect(
        address,
        support::PROXY_SERVER_NAME,
        "ca-a.pem",
        &[b"http/1.1"],
    )
    .await
    .expect("https connection");
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: {AUTHORITY}\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    if !body.is_empty() {
        let _ = write!(head, "content-length: {}\r\n", body.len());
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("request head");
    stream.write_all(body).await.expect("request body");
    let mut raw = Vec::new();
    let _ = timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response in time");
    parse(&raw, method == "HEAD")
}

fn parse(raw: &[u8], head_request: bool) -> Answer {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .expect("status line")
        .split(' ')
        .nth(1)
        .expect("status")
        .parse()
        .expect("code");
    let mut headers = HeaderMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').expect("field");
        headers.append(
            http::HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes()).expect("name"),
            HeaderValue::from_str(value.trim()).expect("value"),
        );
    }
    let rest = &raw[split + 4..];
    let body = if head_request {
        Vec::new()
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|value| value == "chunked")
    {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    Answer {
        status,
        headers,
        body: Bytes::from(body),
        trailers: None,
    }
}

fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("chunk size");
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..line_end]).expect("size"), 16)
            .expect("hex");
        rest = &rest[line_end + 2..];
        if size == 0 {
            return body;
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// The fields two answers must share: the framing and per-connection fields are each protocol's own.
fn comparable(answer: &Answer) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let own = [
        "date",
        "server",
        "x-request-id",
        "x-upstream-request-id",
        "connection",
        "keep-alive",
        "transfer-encoding",
        "content-length",
    ];
    let mut fields = answer
        .headers
        .iter()
        .filter(|(name, _)| !own.contains(&name.as_str()))
        .map(|(name, value)| (name.to_string(), value.to_str().expect("text").to_owned()))
        .collect::<Vec<_>>();
    fields.sort();
    (answer.status, fields, answer.body.to_vec())
}

fn policies() -> Vec<HttpRoute> {
    let response_policy = HttpProxyPolicy {
        response_headers: vec![
            oxiroute_config::HttpResponseHeaderMutation::Set {
                name: "x-added".into(),
                value: "yes".into(),
                always: true,
            },
            oxiroute_config::HttpResponseHeaderMutation::Remove {
                name: "x-origin".into(),
            },
        ],
        ..HttpProxyPolicy::default()
    };
    let forwarding = super::policy::forwarding_policy();
    vec![
        route(
            "/api",
            "origin",
            HttpProxyPolicy {
                upstream_path_rewrite: Some(super::policy::rewrite()),
                ..forwarding.clone()
            },
        ),
        route("/resp", "origin", response_policy),
        route("/", "origin", forwarding),
    ]
}

#[tokio::test]
pub(super) async fn the_same_service_answers_alike_on_an_https_and_an_http3_listener() {
    let (origin, address) = ScriptedOrigin::tcp(echo).await;
    let (h3_address, https_address) = (
        reserve_udp_address(),
        process_support::ports::unused_address(),
    );
    let mut draft = draft(
        h3_address,
        service(policies()),
        vec![tcp_pool("origin", &[address])],
    );
    draft.tls_profiles.push(TlsProfile {
        name: "classic".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls12,
        alpn: vec![AlpnProtocol::Http11],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    draft.listeners.push(Listener {
        name: "classic".into(),
        bind: support::socket_bind(https_address),
        protocol: Protocol::Http,
        service: Some("web".into()),
        tls_profile: Some("classic".into()),
        proxy_protocol: None,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    let config = draft
        .validate()
        .expect("a service shared by an https and an http3 listener");
    let mut daemon = Daemon::start(h3_address, &config).await;
    daemon.server.wait_for_tcp(https_address).await;

    for (method, path, headers, body) in TABLE {
        let by_h3 = daemon.send(method, path, headers, body).await;
        let by_https = classic(https_address, method, path, headers, body).await;
        assert_eq!(comparable(&by_h3), comparable(&by_https), "{method} {path}");
        if *method != "HEAD" && by_h3.status != 204 && by_h3.status != 304 {
            // An answer the origin did not give a length streams: it has none, as on the https listener.
            let length = by_https
                .header("content-length")
                .map(|_| by_h3.body.len().to_string());
            assert_eq!(
                by_h3.header("content-length"),
                length.as_deref(),
                "{method} {path}"
            );
        }
    }
    let requests = origin.requests();
    assert_eq!(requests.len(), 2 * TABLE.len());
    for pair in requests.chunks(2) {
        let (by_h3, by_https) = (&pair[0], &pair[1]);
        assert_eq!(
            (&by_h3.method, &by_h3.target, &by_h3.body),
            (&by_https.method, &by_https.target, &by_https.body)
        );
        for name in [
            "host",
            "x-forwarded-for",
            "x-forwarded-proto",
            "x-real-ip",
            "x-secret",
            "x-custom",
            "content-type",
        ] {
            assert_eq!(
                by_h3.header(name),
                by_https.header(name),
                "{} {} {name}",
                by_h3.method,
                by_h3.target
            );
        }
    }
    daemon.finish().await;
}
