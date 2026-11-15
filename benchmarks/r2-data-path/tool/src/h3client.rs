//! HTTP/3 client of the load generator: one quinn endpoint (UDP socket) per connection group, one h3 connection
//! per keep-alive client, requests multiplexed as bidirectional streams over cloned `Link`s.

use std::{io, net::{Ipv4Addr, SocketAddr}, sync::Arc};

use bytes::{BufMut, Bytes, BytesMut};
use hyper::{Request, Response};
use quinn::{ClientConfig, Endpoint, crypto::rustls::QuicClientConfig};

/// A client endpoint on its own UDP socket that trusts what `tls` trusts (QUIC needs TLS 1.3 and the `h3` ALPN).
pub fn endpoint(tls: rustls::ClientConfig) -> io::Result<Endpoint> {
    let quic = QuicClientConfig::try_from(tls).map_err(io::Error::other)?;
    let mut endpoint = Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(ClientConfig::new(Arc::new(quic)));
    Ok(endpoint)
}

/// One HTTP/3 connection; clones open further streams on it. The connection closes when the last clone is dropped.
#[derive(Clone)]
pub struct Link {
    send: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    connection: quinn::Connection,
}

impl Link {
    pub async fn connect(endpoint: &Endpoint, target: SocketAddr, host: &str) -> io::Result<Self> {
        let connection = endpoint.connect(target, host).map_err(io::Error::other)?.await?;
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .map_err(io::Error::other)?;
        tokio::spawn(async move { std::future::poll_fn(|context| driver.poll_close(context)).await });
        Ok(Self { send, connection })
    }

    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    /// Sends one request (an optional body, then FIN) and collects the answer for content validation.
    pub async fn request(&mut self, request: Request<()>, body: &Bytes) -> io::Result<Response<Bytes>> {
        let mut stream = self.send.send_request(request).await.map_err(io::Error::other)?;
        if !body.is_empty() {
            stream.send_data(body.clone()).await.map_err(io::Error::other)?;
        }
        stream.finish().await.map_err(io::Error::other)?;
        let response = stream.recv_response().await.map_err(io::Error::other)?;
        let mut body = BytesMut::new();
        while let Some(chunk) = stream.recv_data().await.map_err(io::Error::other)? {
            body.put(chunk);
        }
        Ok(response.map(|()| body.freeze()))
    }
}
