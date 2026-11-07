use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use super::*;

fn public_policy() -> RtmpOutboundPolicy {
    RtmpOutboundPolicy {
        deny_private: false,
        ..RtmpOutboundPolicy::default()
    }
}

#[test]
fn callback_url_parses_default_port_and_path() {
    let endpoint = RtmpCallbackEndpoint::parse("http://198.51.100.4/notify", &public_policy())
        .expect("callback endpoint");
    assert_eq!(
        endpoint.address(),
        "198.51.100.4:80".parse().expect("address")
    );
    assert_eq!(endpoint.path, "/notify");
}

#[test]
fn callback_url_rejects_fragments_and_credentials() {
    assert_eq!(
        RtmpCallbackEndpoint::parse("http://user@198.51.100.4/notify", &public_policy()),
        Err(RtmpCallbackError::InvalidUrl)
    );
    assert_eq!(
        RtmpCallbackEndpoint::parse("http://198.51.100.4/notify#fragment", &public_policy()),
        Err(RtmpCallbackError::InvalidUrl)
    );
}

#[test]
fn form_encoding_is_bounded_and_does_not_log_secrets() {
    let context = RtmpCallbackContext {
        service_id: "edge".into(),
        application: "live".into(),
        stream_name: Some("camera".into()),
        query: Some("token=a&b".into()),
        client_addr: Some("192.0.2.10".parse().expect("address")),
        session_id: "session".into(),
    };
    let form = form_parameters(RtmpCallbackEvent::Update, &context).expect("form");
    assert!(form.contains("query=token%3Da%26b"));
    assert!(!format!("{context:?}").contains("secret"));
}

#[test]
fn post_callback_sends_event_fields_and_accepts_success() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("callback listener");
    let address = listener.local_addr().expect("callback address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("callback connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("callback read timeout");
        let mut request = Vec::new();
        let mut buffer = [0; 1_024];
        loop {
            let count = stream.read(&mut buffer).expect("callback request");
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n")
                && request.windows(12).any(|window| window == b"call=publish")
            {
                break;
            }
        }
        assert!(request.starts_with(b"POST /notify HTTP/1.1"));
        assert!(request.windows(12).any(|window| window == b"call=publish"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .expect("callback response");
    });
    let endpoint =
        RtmpCallbackEndpoint::parse(&format!("http://{address}/notify"), &public_policy())
            .expect("callback endpoint");
    let policy = RtmpCallbackPolicy {
        on_publish: Some(endpoint),
        timeout: Duration::from_secs(1),
        ..RtmpCallbackPolicy::default()
    };
    let context = RtmpCallbackContext {
        service_id: "edge".into(),
        application: "live".into(),
        stream_name: Some("camera".into()),
        query: None,
        client_addr: None,
        session_id: "session".into(),
    };
    policy
        .authorize(RtmpCallbackEvent::Publish, &context)
        .expect("callback accepted");
    server.join().expect("callback server");
}
