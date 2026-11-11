//! The reverse listener answers an `h3` crate client and a raw quinn client with the same response.
#[allow(clippy::wildcard_imports)]
use super::*;
use oxiroute_h3::frame;
use oxiroute_h3::qpack::{
    decode::{Line, section},
    static_table::ENTRIES,
};
use oxiroute_h3::varint;

use super::raw::{open_control, raw_request};

type Fields = Vec<(String, String)>;

/// What a client saw: status, fields (sorted, `date` left out because it changes with the clock) and body.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    status: u16,
    fields: Fields,
    body: Vec<u8>,
}

fn fixed(path: &str, status: u16, body: &str, headers: &[(&str, &str)]) -> HttpRoute {
    let mut route = reverse_service(HttpRouteAction::FixedResponse {
        status,
        body: body.into(),
        headers: headers
            .iter()
            .map(|(name, value)| oxiroute_config::HttpLiteralHeader {
                name: (*name).into(),
                value: (*value).into(),
                always: true,
            })
            .collect(),
    })
    .routes
    .remove(0);
    route.path = HttpPathSelector::Exact { value: path.into() };
    route
}

fn redirect_route() -> HttpRoute {
    let mut route = reverse_service(HttpRouteAction::Redirect {
        status: 308,
        location: HttpRedirectLocation::RequestTemplate {
            value: "$scheme://$host/moved$request_uri".into(),
            nginx_host_fallback: None,
        },
        headers: Vec::new(),
    })
    .routes
    .remove(0);
    route.path = HttpPathSelector::Exact {
        value: "/redirect".into(),
    };
    route
}

fn service() -> HttpService {
    let mut service = fixed_service();
    service.routes = vec![
        fixed("/plain", 200, "hello", &[("x-one", "1"), ("x-two", "two")]),
        fixed("/missing", 404, "nope", &[]),
        fixed("/empty", 204, "", &[("x-empty", "yes")]),
        fixed("/big", 200, &"b".repeat(40_000), &[]),
        redirect_route(),
    ];
    service
}

fn sorted(mut fields: Fields) -> Fields {
    fields.retain(|(name, _)| name != "date");
    fields.sort();
    fields
}

async fn seen_by_h3_client(
    sender: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    method: Method,
    path: &str,
) -> Seen {
    let request = Request::builder()
        .method(method)
        .uri(format!("https://example.test{path}"))
        .body(())
        .expect("request");
    let mut stream = sender.send_request(request).await.expect("send request");
    stream.finish().await.expect("finish request");
    let response = stream.recv_response().await.expect("response");
    let fields = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().expect("text").to_owned()))
        .collect();
    Seen {
        status: response.status().as_u16(),
        fields: sorted(fields),
        body: recv_body(&mut stream).await.to_vec(),
    }
}

fn text(string: oxiroute_h3::qpack::decode::Str<'_>) -> String {
    let mut out = Vec::new();
    string.write_to(&mut out).expect("string");
    String::from_utf8(out).expect("text")
}

fn field(line: Line<'_>) -> (String, String) {
    match line {
        Line::Indexed(index) => {
            let (name, value) = ENTRIES[usize::from(index)];
            (name.into(), value.into())
        }
        Line::NameRef { index, value } => (ENTRIES[usize::from(index)].0.into(), text(value)),
        Line::Literal { name, value } => (text(name), text(value)),
    }
}

/// Splits a response stream into frames; the first is `HEADERS`, the data frames are joined.
fn parse_response(bytes: &[u8]) -> Seen {
    let mut src = bytes;
    let mut fields = Vec::new();
    let mut body = Vec::new();
    let mut headers_frames = 0;
    while !src.is_empty() {
        let kind = varint::decode(&mut src).expect("frame type");
        let len = usize::try_from(varint::decode(&mut src).expect("frame length")).expect("length");
        let (payload, rest) = src.split_at(len);
        src = rest;
        match kind {
            frame::HEADERS => {
                headers_frames += 1;
                let mut lines = section(payload).expect("section");
                while let Some(line) = lines.next_line().expect("line") {
                    fields.push(field(line));
                }
            }
            frame::DATA => body.extend_from_slice(payload),
            other => panic!("unexpected frame {other:#x}"),
        }
    }
    assert_eq!(headers_frames, 1, "one HEADERS frame and no trailers");
    let status = fields
        .iter()
        .find(|(name, _)| name == ":status")
        .expect(":status")
        .1
        .parse()
        .expect("status");
    fields.retain(|(name, _)| !name.starts_with(':'));
    Seen {
        status,
        fields: sorted(fields),
        body,
    }
}

async fn raw_bytes(connection: &quinn::Connection, method: &Method, path: &str) -> Vec<u8> {
    let (mut send, mut recv) = connection.open_bi().await.expect("request stream");
    send.write_all(&raw_request(method, path))
        .await
        .expect("request");
    send.finish().expect("finish request");
    recv.read_to_end(1 << 20).await.expect("response")
}

#[tokio::test]
pub(super) async fn reverse_h3_answers_an_h3_crate_client_and_a_raw_quinn_client_identically() {
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = reverse_config(listener_address, key.path(), service(), Vec::new(), Some(8));
    let config = config.validate().expect("valid reverse H3 config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let h3_connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(h3_connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let raw_connection = connect_h3(&endpoint, listener_address).await;
    let _control = open_control(&raw_connection).await;

    for path in [
        "/plain",
        "/missing",
        "/empty",
        "/big",
        "/redirect",
        "/unrouted",
    ] {
        for method in [Method::GET, Method::HEAD] {
            let by_h3 = seen_by_h3_client(&mut sender, method.clone(), path).await;
            let first = raw_bytes(&raw_connection, &method, path).await;
            let second = raw_bytes(&raw_connection, &method, path).await;
            assert_eq!(
                first, second,
                "{method} {path}: the encoding is deterministic"
            );
            assert_eq!(by_h3, parse_response(&first), "{method} {path}");
        }
    }

    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
}
