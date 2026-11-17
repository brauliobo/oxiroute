#![allow(dead_code)]

use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use h3::{
    client::RequestStream,
    error::{Code, StreamError},
};
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::{CertificateDer, pem::PemObject as _};
use tokio::time::{sleep, timeout};

pub const H3_ALPN: &[u8] = b"h3";

pub fn client_endpoint(ca_certificate_path: &Path, alpn: &[u8]) -> io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(ca_certificate_path)?;
    for certificate in CertificateDer::pem_slice_iter(&ca) {
        roots
            .add(certificate.map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    }
    let mut crypto = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(std::io::Error::other)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![alpn.to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).map_err(io::Error::other)?,
    ));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

pub async fn connect(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
    server_name: &str,
) -> quinn::Connection {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(address, server_name)
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("H3 daemon connection timeout")
}

pub async fn recv_chunk<S>(stream: &mut RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut chunk = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    chunk.copy_to_bytes(chunk.remaining())
}

pub async fn try_finish_unless_answered<S>(
    stream: &mut RequestStream<S, Bytes>,
) -> Result<(), StreamError>
where
    S: h3::quic::SendStream<Bytes>,
{
    match stream.finish().await {
        Err(StreamError::RemoteTerminate { code, .. }) if code == Code::H3_NO_ERROR => Ok(()),
        other => other,
    }
}

pub async fn finish_unless_answered<S>(stream: &mut RequestStream<S, Bytes>)
where
    S: h3::quic::SendStream<Bytes>,
{
    if let Err(error) = try_finish_unless_answered(stream).await {
        panic!("finish H3 request: {error:?}");
    }
}

pub fn drive_client<C>(mut driver: h3::client::Connection<C, Bytes>) -> tokio::task::JoinHandle<()>
where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::SendStream: Send,
    C::RecvStream: Send,
{
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    })
}
