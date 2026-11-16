use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{Duration, timeout},
};

use crate::config_support::load_lua;

pub(crate) async fn exchange(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(30), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("proxy response");
        response
    })
    .await
    .expect("proxy exchange timeout")
}

pub(crate) fn forward_cache_config(address: std::net::SocketAddr) -> oxiroute_config::ConfigDraft {
    load_lua(&format!(
        r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{address}" }},
    protocol = "forward_http1",
    service = "forward",
  }} }},
  cache_stores = {{ {{ name = "memory", type = "memory" }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    tls_required = false,
    destination_policy = {{ deny_private = false }},
    cache = {{ store = "memory" }},
  }} }},
}}"#,
    ))
    .expect("forward cache config")
    .to_draft()
}

pub(crate) async fn read_request_head(stream: &mut TcpStream, buffer: &mut [u8]) -> Vec<u8> {
    let mut request = Vec::new();
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(buffer).await.expect("origin request read");
        assert_ne!(read, 0, "origin request ended before headers");
        request.extend_from_slice(&buffer[..read]);
    }
    request
}

pub(crate) async fn exchange_head(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(30), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            response.push(stream.read_u8().await.expect("proxy response byte"));
        }
        response
    })
    .await
    .expect("proxy response head timeout")
}

pub(crate) async fn exchange_partial(address: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(30), async {
        let mut stream = TcpStream::connect(address).await.expect("proxy connect");
        stream.write_all(request).await.expect("proxy request");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        response
    })
    .await
    .expect("proxy partial exchange timeout")
}
