use std::{
    future::Future,
    io,
    net::{Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bytes::{Buf as _, Bytes};
use http::{HeaderMap, Request, StatusCode, header::CONTENT_LENGTH};
use oxiroute_config::UpstreamPool;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Endpoint, TransportConfig, VarInt};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, pem::PemObject},
};
use tokio::{
    sync::{OnceCell, OwnedSemaphorePermit, Semaphore, watch},
    time::{Instant, timeout, timeout_at},
};

pub(crate) const H3_UPSTREAM_MAX_FIELD_SECTION_BYTES: u64 = 16 * 1024;
pub(crate) const H3_UPSTREAM_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const H3_UPSTREAM_MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const H3_UPSTREAM_MAX_CONNECTIONS: usize = 64;
const H3_UPSTREAM_BIDI_STREAM_LIMIT: u32 = 128;
const H3_UPSTREAM_UNI_STREAM_LIMIT: u32 = 16;
const H3_UPSTREAM_STREAM_RECEIVE_WINDOW: u32 = 1024 * 1024;
const H3_UPSTREAM_CONNECTION_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
const H3_UPSTREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const H3_CLOSE_CODE: VarInt = VarInt::from_u32(0);

#[derive(Debug, thiserror::Error)]
pub enum H3UpstreamBuildError {
    #[error("upstream TLS policy could not be prepared")]
    Tls(#[source] Box<crate::tls::TlsBuildError>),
    #[error("native TLS roots could not be loaded")]
    NativeRoots(#[source] io::Error),
    #[error("upstream pool `{pool}` has no usable TLS roots")]
    EmptyRoots { pool: String },
    #[error("upstream pool `{pool}` custom CA bundle `{path}` could not be parsed")]
    CustomCaParse { pool: String, path: PathBuf },
    #[error("upstream pool `{pool}` has an invalid QUIC TLS configuration: {detail}")]
    QuicTls { pool: String, detail: String },
    #[error("upstream pool `{pool}` has an invalid QUIC transport configuration: {detail}")]
    QuicTransport { pool: String, detail: String },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum H3UpstreamError {
    #[error("HTTP/3 upstream connection failed")]
    Connect,
    #[error("HTTP/3 upstream connection timed out")]
    Timeout,
    #[error("HTTP/3 upstream request was cancelled")]
    Cancelled,
    #[error("HTTP/3 upstream stream was refused")]
    RefusedStream,
    #[error("HTTP/3 upstream protocol negotiation failed")]
    Protocol,
    #[error("HTTP/3 upstream response body exceeded the configured limit")]
    ResponseBodyTooLarge,
    #[error("HTTP/3 upstream request body exceeded the configured limit")]
    RequestBodyTooLarge,
    #[error("HTTP/3 upstream resource limit was exhausted")]
    ResourceExhausted,
    #[error("HTTP/3 upstream server name is unavailable")]
    MissingServerName,
}

impl H3UpstreamError {
    pub(crate) const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Connect | Self::Timeout | Self::RefusedStream | Self::Protocol
        )
    }
}

#[derive(Debug)]
pub(crate) struct H3UpstreamResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) trailers: Option<HeaderMap>,
}

pub(crate) struct H3UpstreamPlan {
    client_config: ClientConfig,
    endpoint: OnceCell<Endpoint>,
    connections: Arc<Semaphore>,
    server_name: Option<String>,
}

impl std::fmt::Debug for H3UpstreamPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("H3UpstreamPlan")
            .field("server_name", &self.server_name)
            .field("max_connections", &self.connections.available_permits())
            .finish_non_exhaustive()
    }
}

impl H3UpstreamPlan {
    pub(crate) fn from_pool(pool: &UpstreamPool) -> Result<Option<Self>, H3UpstreamBuildError> {
        if pool.http_versions.min != oxiroute_config::HttpVersion::Http3 {
            return Ok(None);
        }
        let Some(tls) = &pool.tls else {
            return Err(H3UpstreamBuildError::EmptyRoots {
                pool: pool.name.clone(),
            });
        };
        let roots = match tls.ca_certificate_path.as_deref() {
            Some(path) => custom_roots(&pool.name, path)?,
            None => native_roots(&pool.name)?,
        };
        Self::new(
            pool.name.clone(),
            roots,
            Some(tls.server_name.to_ascii_lowercase()),
            H3_UPSTREAM_MAX_CONNECTIONS,
        )
        .map(Some)
    }

    pub(crate) fn for_forward(max_connections: usize) -> Result<Self, H3UpstreamBuildError> {
        let roots = native_roots("forward HTTP/3")?;
        Self::new(
            "forward HTTP/3".into(),
            roots,
            None,
            max_connections.clamp(1, H3_UPSTREAM_MAX_CONNECTIONS),
        )
    }

    fn new(
        owner: String,
        roots: RootCertStore,
        server_name: Option<String>,
        max_connections: usize,
    ) -> Result<Self, H3UpstreamBuildError> {
        if roots.is_empty() {
            return Err(H3UpstreamBuildError::EmptyRoots { pool: owner });
        }
        let mut crypto =
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_root_certificates(roots)
                .with_no_client_auth();
        crypto.alpn_protocols = vec![b"h3".to_vec()];
        crypto.enable_early_data = false;
        let quic =
            QuicClientConfig::try_from(crypto).map_err(|error| H3UpstreamBuildError::QuicTls {
                pool: owner.clone(),
                detail: error.to_string(),
            })?;
        let mut client_config = ClientConfig::new(Arc::new(quic));
        let idle_timeout =
            quinn::IdleTimeout::try_from(H3_UPSTREAM_IDLE_TIMEOUT).map_err(|error| {
                H3UpstreamBuildError::QuicTransport {
                    pool: owner.clone(),
                    detail: error.to_string(),
                }
            })?;
        let mut transport = TransportConfig::default();
        transport
            .max_concurrent_bidi_streams(VarInt::from_u32(H3_UPSTREAM_BIDI_STREAM_LIMIT))
            .max_concurrent_uni_streams(VarInt::from_u32(H3_UPSTREAM_UNI_STREAM_LIMIT))
            .max_idle_timeout(Some(idle_timeout))
            .stream_receive_window(VarInt::from_u32(H3_UPSTREAM_STREAM_RECEIVE_WINDOW))
            .receive_window(VarInt::from_u32(H3_UPSTREAM_CONNECTION_RECEIVE_WINDOW))
            .allow_spin(false)
            .datagram_receive_buffer_size(None)
            .datagram_send_buffer_size(0);
        client_config.transport_config(Arc::new(transport));
        Ok(Self {
            client_config,
            endpoint: OnceCell::new(),
            connections: Arc::new(Semaphore::new(max_connections)),
            server_name,
        })
    }

    pub(crate) fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }

    pub(crate) async fn request(
        &self,
        address: SocketAddr,
        server_name: &str,
        request: Request<()>,
        body: Bytes,
        deadline: Instant,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<H3UpstreamResponse, H3UpstreamError> {
        if body.len() > H3_UPSTREAM_MAX_REQUEST_BODY_BYTES {
            return Err(H3UpstreamError::RequestBodyTooLarge);
        }
        if server_name.is_empty() {
            return Err(H3UpstreamError::MissingServerName);
        }
        let _connection_permit =
            acquire_connection(Arc::clone(&self.connections), deadline, &mut shutdown).await?;
        let endpoint = self.endpoint(deadline, &mut shutdown).await?;
        let connecting = endpoint
            .connect(address, server_name)
            .map_err(|_| H3UpstreamError::Connect)?;
        let connection = wait_result(
            async move { connecting.await.map_err(|_| H3UpstreamError::Connect) },
            deadline,
            &mut shutdown,
        )
        .await?;
        let raw_connection = connection.clone();
        let mut builder = h3::client::builder();
        builder
            .max_field_section_size(H3_UPSTREAM_MAX_FIELD_SECTION_BYTES)
            .send_grease(false);
        let (mut driver, mut sender) = wait_result(
            async move {
                builder
                    .build::<_, _, Bytes>(h3_quinn::Connection::new(connection))
                    .await
                    .map_err(|_| H3UpstreamError::Protocol)
            },
            deadline,
            &mut shutdown,
        )
        .await?;
        let driver_task = tokio::spawn(async move {
            let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
        });
        let exchange = exchange(&mut sender, request, body);
        let result = tokio::select! {
            changed = shutdown.changed() => {
                let _ = changed;
                Err(H3UpstreamError::Cancelled)
            }
            result = timeout_at(deadline, exchange) => {
                result.map_err(|_| H3UpstreamError::Timeout)?
            }
        };
        raw_connection.close(H3_CLOSE_CODE, b"request complete");
        let mut driver_task = driver_task;
        let _ = timeout(Duration::from_secs(1), &mut driver_task).await;
        driver_task.abort();
        result
    }

    async fn endpoint(
        &self,
        deadline: Instant,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<Endpoint, H3UpstreamError> {
        let client_config = self.client_config.clone();
        let endpoint = wait_result(
            self.endpoint.get_or_try_init(|| async move {
                let mut endpoint = Endpoint::client((Ipv6Addr::UNSPECIFIED, 0).into())
                    .map_err(|_| H3UpstreamError::Connect)?;
                endpoint.set_default_client_config(client_config);
                Ok::<Endpoint, H3UpstreamError>(endpoint)
            }),
            deadline,
            shutdown,
        )
        .await?;
        Ok(endpoint.clone())
    }
}

async fn exchange(
    sender: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    request: Request<()>,
    body: Bytes,
) -> Result<H3UpstreamResponse, H3UpstreamError> {
    let request_method = request.method().clone();
    let mut stream = sender
        .send_request(request)
        .await
        .map_err(|_| H3UpstreamError::RefusedStream)?;
    if !body.is_empty() {
        stream
            .send_data(body)
            .await
            .map_err(|_| H3UpstreamError::RefusedStream)?;
    }
    stream
        .finish()
        .await
        .map_err(|_| H3UpstreamError::RefusedStream)?;
    let response = stream
        .recv_response()
        .await
        .map_err(|_| H3UpstreamError::Protocol)?;
    let expected_length = response
        .headers()
        .get_all(CONTENT_LENGTH)
        .iter()
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(H3UpstreamError::Protocol)
        })
        .transpose()?;
    if response
        .headers()
        .get_all(CONTENT_LENGTH)
        .iter()
        .nth(1)
        .is_some()
    {
        return Err(H3UpstreamError::Protocol);
    }
    if expected_length.is_some_and(|length| {
        length > u64::try_from(H3_UPSTREAM_MAX_RESPONSE_BODY_BYTES).unwrap_or(u64::MAX)
    }) {
        stream.stop_sending(h3::error::Code::H3_EXCESSIVE_LOAD);
        stream.stop_stream(h3::error::Code::H3_EXCESSIVE_LOAD);
        return Err(H3UpstreamError::ResponseBodyTooLarge);
    }
    let mut body = Vec::new();
    while let Some(mut chunk) = stream
        .recv_data()
        .await
        .map_err(|_| H3UpstreamError::Protocol)?
    {
        let length = chunk.remaining();
        let chunk = chunk.copy_to_bytes(length);
        if body.len().saturating_add(chunk.len()) > H3_UPSTREAM_MAX_RESPONSE_BODY_BYTES {
            stream.stop_sending(h3::error::Code::H3_EXCESSIVE_LOAD);
            stream.stop_stream(h3::error::Code::H3_EXCESSIVE_LOAD);
            return Err(H3UpstreamError::ResponseBodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    if request_method != http::Method::HEAD
        && expected_length
            .is_some_and(|length| length != u64::try_from(body.len()).unwrap_or(u64::MAX))
    {
        return Err(H3UpstreamError::Protocol);
    }
    let trailers = stream
        .recv_trailers()
        .await
        .map_err(|_| H3UpstreamError::Protocol)?;
    Ok(H3UpstreamResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: Bytes::from(body),
        trailers,
    })
}

async fn acquire_connection(
    connections: Arc<Semaphore>,
    deadline: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<OwnedSemaphorePermit, H3UpstreamError> {
    wait_result(
        async move {
            connections
                .acquire_owned()
                .await
                .map_err(|_| H3UpstreamError::ResourceExhausted)
        },
        deadline,
        shutdown,
    )
    .await
}

async fn wait_result<F, T>(
    future: F,
    deadline: Instant,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<T, H3UpstreamError>
where
    F: Future<Output = Result<T, H3UpstreamError>>,
{
    tokio::select! {
        changed = shutdown.changed() => {
            let _ = changed;
            Err(H3UpstreamError::Cancelled)
        }
        result = timeout_at(deadline, future) => result.unwrap_or(Err(H3UpstreamError::Timeout))
    }
}

fn native_roots(owner: &str) -> Result<RootCertStore, H3UpstreamBuildError> {
    let native_certificates = rustls_native_certs::load_native_certs();
    if native_certificates.certs.is_empty()
        && let Some(error) = native_certificates.errors.into_iter().next()
    {
        return Err(H3UpstreamBuildError::NativeRoots(io::Error::other(error)));
    }
    let mut roots = RootCertStore::empty();
    for certificate in native_certificates.certs {
        roots
            .add(certificate)
            .map_err(|error| H3UpstreamBuildError::QuicTls {
                pool: owner.into(),
                detail: error.to_string(),
            })?;
    }
    Ok(roots)
}

fn custom_roots(owner: &str, path: &Path) -> Result<RootCertStore, H3UpstreamBuildError> {
    let pem = crate::tls::read_bounded_stable(
        owner,
        "upstream CA bundle",
        path,
        crate::tls::MAX_CA_CERTIFICATE_BYTES,
        false,
    )
    .map_err(|error| H3UpstreamBuildError::Tls(Box::new(error)))?;
    let mut roots = RootCertStore::empty();
    let mut certificates = 0_usize;
    for certificate in CertificateDer::pem_slice_iter(pem.as_slice()) {
        let certificate = certificate.map_err(|_| H3UpstreamBuildError::CustomCaParse {
            pool: owner.into(),
            path: path.into(),
        })?;
        roots
            .add(certificate)
            .map_err(|_| H3UpstreamBuildError::CustomCaParse {
                pool: owner.into(),
                path: path.into(),
            })?;
        certificates = certificates.saturating_add(1);
    }
    if certificates == 0 {
        return Err(H3UpstreamBuildError::CustomCaParse {
            pool: owner.into(),
            path: path.into(),
        });
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        io::BufReader,
        net::{Ipv4Addr, SocketAddr},
        path::PathBuf,
        sync::Arc,
        time::Duration,
    };

    use bytes::{Buf as _, Bytes};
    use h3::server::Connection;
    use http::{HeaderMap, Method, Request, Response, StatusCode};
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::PrivateKeyDer;
    use tokio::sync::watch;

    use super::*;

    const ORIGIN_SERVER_NAME: &str = "origin.example.test";

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(
            option_env!("OXIROUTE_SERVER_MANIFEST_DIR").unwrap_or(env!("CARGO_MANIFEST_DIR")),
        )
        .join("tests/fixtures")
        .join(name)
    }

    fn test_plan(max_connections: usize) -> H3UpstreamPlan {
        let roots =
            custom_roots("HTTP/3 upstream test", &fixture("ca-a.pem")).expect("test CA bundle");
        H3UpstreamPlan::new(
            "HTTP/3 upstream test".into(),
            roots,
            Some(ORIGIN_SERVER_NAME.into()),
            max_connections,
        )
        .expect("HTTP/3 client plan")
    }

    fn origin_server_config(alpn: &[u8]) -> quinn::ServerConfig {
        let mut certificate_reader =
            BufReader::new(File::open(fixture("origin.pem")).expect("origin certificate fixture"));
        let certificates = CertificateDer::pem_reader_iter(&mut certificate_reader)
            .collect::<Result<Vec<_>, _>>()
            .expect("origin certificate chain");
        let mut key_reader = BufReader::new(
            File::open(fixture("origin-key.pem")).expect("origin private-key fixture"),
        );
        let private_key = PrivateKeyDer::pem_reader_iter(&mut key_reader)
            .next()
            .transpose()
            .expect("origin private key")
            .expect("origin private key block");
        let mut crypto =
            rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_no_client_auth()
                .with_single_cert(certificates, private_key)
                .expect("origin TLS identity");
        crypto.alpn_protocols = vec![alpn.to_vec()];
        crypto.max_early_data_size = 0;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).expect("QUIC TLS configuration"),
        ));
        config.migration(false);
        config
    }

    #[tokio::test]
    async fn round_trips_h3_request_body_response_and_trailers() {
        let endpoint =
            quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
                .expect("origin endpoint");
        let address = endpoint.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("origin incoming connection");
            let connection = incoming.await.expect("origin QUIC connection");
            let mut h3: Connection<_, Bytes> = h3::server::builder()
                .build(h3_quinn::Connection::new(connection))
                .await
                .expect("origin H3 connection");
            let resolver = h3
                .accept()
                .await
                .expect("origin H3 accept")
                .expect("origin H3 request");
            let (request, mut stream) = resolver.resolve_request().await.expect("origin request");
            assert_eq!(request.method(), Method::POST);
            assert_eq!(request.uri().path(), "/h3");
            assert_eq!(
                request.uri().authority().map(http::uri::Authority::as_str),
                Some(ORIGIN_SERVER_NAME)
            );
            let mut body = Vec::new();
            while let Some(mut chunk) = stream.recv_data().await.expect("origin request body") {
                let length = chunk.remaining();
                body.extend_from_slice(&chunk.copy_to_bytes(length));
            }
            assert_eq!(body, b"request-body");
            stream
                .send_response(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("x-origin", "h3")
                        .body(())
                        .expect("origin response"),
                )
                .await
                .expect("origin response headers");
            stream
                .send_data(Bytes::from_static(b"response-body"))
                .await
                .expect("origin response body");
            let mut trailers = HeaderMap::new();
            trailers.insert(
                "x-origin-trailer",
                "complete".parse().expect("trailer value"),
            );
            stream
                .send_trailers(trailers)
                .await
                .expect("origin response trailers");
            stream.finish().await.expect("origin response finish");
            let _ = h3.accept().await;
        });

        let plan = test_plan(1);
        let request = Request::builder()
            .method(Method::POST)
            .uri("https://origin.example.test/h3")
            .body(())
            .expect("upstream request");
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let response = plan
            .request(
                address,
                ORIGIN_SERVER_NAME,
                request,
                Bytes::from_static(b"request-body"),
                Instant::now() + Duration::from_secs(5),
                shutdown,
            )
            .await;
        origin.await.expect("origin task");
        let response = response.expect("H3 upstream response");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.headers["x-origin"], "h3");
        assert_eq!(response.body, Bytes::from_static(b"response-body"));
        assert_eq!(
            response.trailers.expect("response trailers")["x-origin-trailer"],
            "complete"
        );
        assert_eq!(plan.connections.available_permits(), 1);
    }

    #[tokio::test]
    async fn rejects_a_mismatched_upstream_content_length() {
        let endpoint =
            quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
                .expect("origin endpoint");
        let address = endpoint.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("origin incoming connection");
            let connection = incoming.await.expect("origin QUIC connection");
            let mut h3: Connection<_, Bytes> = h3::server::builder()
                .build(h3_quinn::Connection::new(connection))
                .await
                .expect("origin H3 connection");
            let resolver = h3
                .accept()
                .await
                .expect("origin H3 accept")
                .expect("origin H3 request");
            let (_request, mut stream) = resolver.resolve_request().await.expect("origin request");
            assert!(
                stream
                    .recv_data()
                    .await
                    .expect("origin request body")
                    .is_none()
            );
            stream
                .send_response(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-length", "4")
                        .body(())
                        .expect("origin response"),
                )
                .await
                .expect("origin response headers");
            stream
                .send_data(Bytes::from_static(b"bad"))
                .await
                .expect("origin response body");
            stream.finish().await.expect("origin response finish");
            let _ = h3.accept().await;
        });

        let plan = test_plan(1);
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let error = plan
            .request(
                address,
                ORIGIN_SERVER_NAME,
                Request::builder()
                    .method(Method::GET)
                    .uri("https://origin.example.test/mismatch")
                    .body(())
                    .expect("upstream request"),
                Bytes::new(),
                Instant::now() + Duration::from_secs(5),
                shutdown,
            )
            .await
            .expect_err("mismatched response length");
        assert!(matches!(error, H3UpstreamError::Protocol));
        origin.await.expect("origin task");
    }

    #[tokio::test]
    async fn preserves_head_response_content_length_without_a_body() {
        let endpoint =
            quinn::Endpoint::server(origin_server_config(b"h3"), (Ipv4Addr::LOCALHOST, 0).into())
                .expect("origin endpoint");
        let address = endpoint.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("origin incoming connection");
            let connection = incoming.await.expect("origin QUIC connection");
            let mut h3: Connection<_, Bytes> = h3::server::builder()
                .build(h3_quinn::Connection::new(connection))
                .await
                .expect("origin H3 connection");
            let resolver = h3
                .accept()
                .await
                .expect("origin H3 accept")
                .expect("origin H3 request");
            let (_request, mut stream) = resolver.resolve_request().await.expect("origin request");
            assert!(
                stream
                    .recv_data()
                    .await
                    .expect("origin request body")
                    .is_none()
            );
            stream
                .send_response(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-length", "7")
                        .body(())
                        .expect("origin response"),
                )
                .await
                .expect("origin response headers");
            stream.finish().await.expect("origin response finish");
            let _ = h3.accept().await;
        });

        let plan = test_plan(1);
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let response = plan
            .request(
                address,
                ORIGIN_SERVER_NAME,
                Request::builder()
                    .method(Method::HEAD)
                    .uri("https://origin.example.test/head")
                    .body(())
                    .expect("upstream request"),
                Bytes::new(),
                Instant::now() + Duration::from_secs(5),
                shutdown,
            )
            .await
            .expect("HEAD upstream response");
        assert_eq!(response.headers[CONTENT_LENGTH], "7");
        assert!(response.body.is_empty());
        origin.await.expect("origin task");
    }

    #[tokio::test]
    async fn rejects_an_upstream_that_does_not_negotiate_h3() {
        let endpoint = quinn::Endpoint::server(
            origin_server_config(b"http/1.1"),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .expect("origin endpoint");
        let address = endpoint.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("origin incoming connection");
            assert!(incoming.await.is_err(), "ALPN mismatch must reject QUIC");
        });
        let plan = test_plan(1);
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let error = plan
            .request(
                address,
                ORIGIN_SERVER_NAME,
                Request::new(()),
                Bytes::new(),
                Instant::now() + Duration::from_secs(5),
                shutdown,
            )
            .await
            .expect_err("non-H3 origin must be rejected");
        assert!(matches!(
            error,
            H3UpstreamError::Connect | H3UpstreamError::Protocol
        ));
        origin.await.expect("origin task");
    }

    #[tokio::test]
    async fn rejects_an_oversized_request_before_connection_admission() {
        let plan = test_plan(1);
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let error = plan
            .request(
                SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
                ORIGIN_SERVER_NAME,
                Request::new(()),
                Bytes::from(vec![0; H3_UPSTREAM_MAX_REQUEST_BODY_BYTES + 1]),
                Instant::now() + Duration::from_secs(1),
                shutdown,
            )
            .await
            .expect_err("oversized request body");
        assert!(matches!(error, H3UpstreamError::RequestBodyTooLarge));
        assert_eq!(plan.connections.available_permits(), 1);
    }

    #[tokio::test]
    async fn cancellation_interrupts_upstream_connection_admission() {
        let plan = test_plan(1);
        let (shutdown_sender, shutdown) = watch::channel(false);
        shutdown_sender.send(true).expect("shutdown watcher");
        let error = plan
            .request(
                SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
                ORIGIN_SERVER_NAME,
                Request::new(()),
                Bytes::new(),
                Instant::now() + Duration::from_secs(1),
                shutdown,
            )
            .await
            .expect_err("cancelled upstream request");
        assert!(matches!(error, H3UpstreamError::Cancelled));
    }

    #[tokio::test]
    async fn timeout_interrupts_upstream_connection_admission() {
        let plan = test_plan(1);
        let permit = plan
            .connections
            .clone()
            .try_acquire_owned()
            .expect("connection permit");
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let error = plan
            .request(
                SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
                ORIGIN_SERVER_NAME,
                Request::new(()),
                Bytes::new(),
                Instant::now() + Duration::from_millis(1),
                shutdown,
            )
            .await
            .expect_err("timed out upstream request");
        drop(permit);
        assert!(matches!(error, H3UpstreamError::Timeout));
    }
}
