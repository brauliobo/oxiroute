use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::read_request_head as read_request_head_bytes;
use oxiroute_server::RoundRobinPool;
use std::io;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(crate) async fn read_request_head(stream: &mut TcpStream) -> io::Result<()> {
    read_request_head_from(stream).await
}

pub(crate) async fn read_request_head_from<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncRead + Unpin,
{
    read_request_head_bytes(stream).await.map(|_| ())
}

pub(crate) async fn read_framed_response(stream: &mut TcpStream) -> io::Result<RawResponse> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    let body_length = loop {
        if let Some(header_end) = response.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&response[..header_end])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Content-Length"))?;
            break (header_end + 4, length);
        }
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    };
    while response.len() < body_length.0 + body_length.1 {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    Ok(RawResponse::parse(response))
}

pub(crate) async fn read_response_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        response.push(byte[0]);
    }
    Ok(response)
}

pub(crate) async fn wait_for_queued_requests(pool: &RoundRobinPool, expected: u64) {
    let converged = timeout(Duration::from_millis(200), async {
        while pool.health_snapshot().queued != expected {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        converged.is_ok(),
        "upstream queue count did not converge to {expected}: {:?}",
        pool.health_snapshot()
    );
}

pub(crate) fn assert_origin_response(response: &RawResponse, origin: &str) {
    assert_eq!(response.status, 200, "response: {}", response.text());
    assert!(
        response.text().ends_with(origin),
        "expected response from {origin}, got: {}",
        response.text()
    );
}
