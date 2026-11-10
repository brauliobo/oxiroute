//! The response header handed to an HTTP/2 stream moves into the frame instead of being copied.

use super::server::{handshake, H2Accept, HttpSession};
use crate::protocols::Digest;
use http::{Method, Request};
use pingora_http::ResponseHeader;
use std::sync::Arc;
use tokio::io::duplex;

#[tokio::test]
async fn response_headers_reach_the_client_and_the_session_keeps_the_status() {
    let (client, server) = duplex(65536);

    let client_task = tokio::spawn(async move {
        let (h2, connection) = h2::client::handshake(client).await.unwrap();
        tokio::spawn(connection);
        let mut h2 = h2.ready().await.unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://www.example.com/")
            .body(())
            .unwrap();
        let (response, _) = h2.send_request(request, true).unwrap();
        let (head, mut body) = response.await.unwrap().into_parts();
        assert_eq!(head.status, 201);
        assert_eq!(head.headers["x-test"], "yes");
        assert_eq!(head.headers["content-type"], "text/plain");
        assert!(head.headers.contains_key("date"));
        assert!(!head.headers.contains_key("connection"));
        assert_eq!(body.data().await.unwrap().unwrap(), "body");
    });

    let mut connection = handshake(Box::new(server), None).await.unwrap();
    let H2Accept::Session(mut http) =
        HttpSession::from_h2_conn(&mut connection, Arc::new(Digest::default()))
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("request was rejected");
    };
    let server_task = tokio::spawn(async move {
        let mut header = ResponseHeader::build(201, None).unwrap();
        header.insert_header("x-test", "yes").unwrap();
        header.insert_header("content-type", "text/plain").unwrap();
        header.insert_header("connection", "keep-alive").unwrap();
        http.write_response_header(Box::new(header), false).unwrap();
        assert_eq!(http.response_written().unwrap().status, 201);
        http.write_body("body".into(), true).await.unwrap();
        assert_eq!(http.body_bytes_sent(), 4);
    });
    // The connection task drives the frames to the client.
    let drive = tokio::spawn(async move { while connection.accept().await.is_some() {} });

    client_task.await.unwrap();
    server_task.await.unwrap();
    drive.abort();
}
