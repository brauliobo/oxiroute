#[derive(Default)]
pub struct OriginObservations {
    accepted: AtomicUsize,
    completed_handshakes: AtomicUsize,
    h2_send_errors: AtomicUsize,
    http_requests: AtomicUsize,
    request_body_bytes: AtomicUsize,
    http_bytes: AtomicUsize,
    request_heads: Mutex<Vec<Vec<u8>>>,
    server_names: Mutex<Vec<String>>,
    alpn: Mutex<Vec<Vec<u8>>>,
    tls_versions: Mutex<Vec<String>>,
    cipher_suites: Mutex<Vec<String>>,
}

impl OriginObservations {
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn http_requests(&self) -> usize {
        self.http_requests.load(Ordering::SeqCst)
    }

    pub fn h2_send_errors(&self) -> usize {
        self.h2_send_errors.load(Ordering::SeqCst)
    }

    pub fn request_body_bytes(&self) -> usize {
        self.request_body_bytes.load(Ordering::SeqCst)
    }

    pub fn http_bytes(&self) -> usize {
        self.http_bytes.load(Ordering::SeqCst)
    }

    pub fn request_heads(&self) -> Vec<Vec<u8>> {
        self.request_heads
            .lock()
            .expect("origin request-head observations")
            .clone()
    }

    pub fn server_names(&self) -> Vec<String> {
        self.server_names
            .lock()
            .expect("server-name observations")
            .clone()
    }

    pub fn alpn(&self) -> Vec<Vec<u8>> {
        self.alpn.lock().expect("ALPN observations").clone()
    }

    pub fn tls_versions(&self) -> Vec<String> {
        self.tls_versions
            .lock()
            .expect("TLS version observations")
            .clone()
    }

    pub fn cipher_suites(&self) -> Vec<String> {
        self.cipher_suites
            .lock()
            .expect("cipher suite observations")
            .clone()
    }

    pub async fn wait_for_completed_handshakes(&self, expected: usize) {
        wait_for_counter(
            &self.completed_handshakes,
            expected,
            "origin TLS handshake attempts",
        )
        .await;
    }

    pub async fn wait_for_http_requests(&self, expected: usize) {
        wait_for_counter(&self.http_requests, expected, "TLS origin HTTP requests").await;
    }

    fn record_tls(&self, stream: &ServerTlsStream<TcpStream>) {
        let connection = stream.get_ref().1;
        if let Some(server_name) = connection.server_name() {
            self.server_names
                .lock()
                .expect("server-name observations")
                .push(server_name.to_owned());
        }
        if let Some(alpn) = connection.alpn_protocol() {
            self.alpn
                .lock()
                .expect("ALPN observations")
                .push(alpn.to_vec());
        }
        if let Some(version) = connection.protocol_version() {
            self.tls_versions
                .lock()
                .expect("TLS version observations")
                .push(format!("{version:?}"));
        }
        if let Some(suite) = connection.negotiated_cipher_suite() {
            self.cipher_suites
                .lock()
                .expect("cipher suite observations")
                .push(format!("{:?}", suite.suite()));
        }
    }
}

pub struct TlsOrigin {
    pub address: SocketAddr,
    pub observations: Arc<OriginObservations>,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), BoxError>>>,
}

impl TlsOrigin {
    pub async fn start_h1(body: &'static [u8]) -> Self {
        let config = tls_server_config(&[b"http/1.1".to_vec()]);
        Self::start(config, move |stream, observations| {
            Box::pin(serve_tls_h1(stream, body, observations))
        })
        .await
    }

    pub async fn start_h2() -> Self {
        let config = tls_server_config(&[b"h2".to_vec()]);
        Self::start(config, |stream, observations| {
            Box::pin(serve_tls_h2(stream, observations))
        })
        .await
    }

    pub async fn start_h2_refusing_streams() -> Self {
        let config = tls_server_config(&[b"h2".to_vec()]);
        Self::start(config, |stream, observations| {
            Box::pin(serve_tls_h2_refusing_streams(stream, observations))
        })
        .await
    }

    pub async fn start_h1_tls12(body: &'static [u8]) -> Self {
        let config =
            tls_server_config_with_versions(&[b"http/1.1".to_vec()], &[&rustls::version::TLS12]);
        Self::start(config, move |stream, observations| {
            Box::pin(serve_tls_h1(stream, body, observations))
        })
        .await
    }

    pub async fn start_h1_with_identity(
        body: &'static [u8],
        certificate_chain_path: &Path,
        private_key_path: &Path,
    ) -> Self {
        let config = tls_server_config_with_identity(
            &[b"http/1.1".to_vec()],
            certificate_chain_path,
            private_key_path,
        );
        Self::start(config, move |stream, observations| {
            Box::pin(serve_tls_h1(stream, body, observations))
        })
        .await
    }

    async fn start<F>(config: ServerConfig, serve: F) -> Self
    where
        F: Fn(
                ServerTlsStream<TcpStream>,
                Arc<OriginObservations>,
            ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("TLS origin bind");
        let address = listener.local_addr().expect("TLS origin address");
        let observations = Arc::new(OriginObservations::default());
        let observations_for_task = Arc::clone(&observations);
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let serve = Arc::new(serve);
        let (shutdown, mut shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        observations_for_task.accepted.fetch_add(1, Ordering::SeqCst);
                        let observations = Arc::clone(&observations_for_task);
                        let acceptor = acceptor.clone();
                        let serve = Arc::clone(&serve);
                        connections.spawn(async move {
                            let accepted = timeout(IO_TIMEOUT, acceptor.accept(stream)).await;
                            observations.completed_handshakes.fetch_add(1, Ordering::SeqCst);
                            let Ok(Ok(stream)) = accepted else {
                                return Ok(());
                            };
                            observations.record_tls(&stream);
                            serve(stream, observations).await
                        });
                    }
                    changed = shutdown_watch.changed() => {
                        changed?;
                        break;
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("TLS connection task exists")??;
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            Ok(())
        });
        Self {
            address,
            observations,
            shutdown,
            task: Some(task),
        }
    }

    pub async fn finish(mut self) {
        self.shutdown.send(true).expect("signal TLS origin");
        finish_origin_task(self.task.take().expect("TLS origin task")).await;
    }
}

impl Drop for TlsOrigin {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve_tls_h1(
    mut stream: ServerTlsStream<TcpStream>,
    body: &'static [u8],
    observations: Arc<OriginObservations>,
) -> Result<(), BoxError> {
    loop {
        match read_until_headers_observed(&mut stream, &observations.http_bytes).await {
            Ok(request) => {
                observations.http_requests.fetch_add(1, Ordering::SeqCst);
                observations
                    .request_heads
                    .lock()
                    .expect("origin request-head observations")
                    .push(request);
                write_h1_response(&mut stream, body).await?;
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

async fn read_until_headers_observed<S>(
    stream: &mut S,
    observed_bytes: &AtomicUsize,
) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    while find_header_end(&bytes).is_none() {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream ended before HTTP headers",
            ));
        }
        observed_bytes.fetch_add(count, Ordering::SeqCst);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

async fn serve_tls_h2(
    stream: ServerTlsStream<TcpStream>,
    observations: Arc<OriginObservations>,
) -> Result<(), BoxError> {
    let mut connection = h2::server::handshake(stream).await?;
    while let Some(request) = connection.accept().await {
        let (request, respond) = request?;
        observations.http_requests.fetch_add(1, Ordering::SeqCst);
        observations
            .http_bytes
            .fetch_add(request.uri().path().len(), Ordering::SeqCst);
        let path = request.uri().path().to_owned();
        if path == "/grpc/upload" {
            let mut body = request.into_body();
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                observations
                    .request_body_bytes
                    .fetch_add(chunk.len(), Ordering::SeqCst);
                body.flow_control().release_capacity(chunk.len())?;
            }
        }
        if respond_h2(&path, respond).await.is_err() {
            observations.h2_send_errors.fetch_add(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

async fn serve_tls_h2_refusing_streams(
    stream: ServerTlsStream<TcpStream>,
    observations: Arc<OriginObservations>,
) -> Result<(), BoxError> {
    let mut connection = h2::server::handshake(stream).await?;
    while let Some(request) = connection.accept().await {
        let (request, mut respond) = request?;
        observations.http_requests.fetch_add(1, Ordering::SeqCst);
        observations
            .http_bytes
            .fetch_add(request.uri().path().len(), Ordering::SeqCst);
        respond.send_reset(h2::Reason::REFUSED_STREAM);
    }
    Ok(())
}

async fn respond_h2(
    path: &str,
    mut respond: h2::server::SendResponse<Bytes>,
) -> Result<(), BoxError> {
    match path {
        "/h2" => {
            let body = Bytes::from_static(b"h2-origin");
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/plain")
                .header(header::CONTENT_LENGTH, body.len())
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            stream.send_data(body, true)?;
        }
        "/grpc/stream" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            for body in [
                Bytes::from_static(b"\0\0\0\0\x05hello"),
                Bytes::from_static(b"\0\0\0\0\x05world"),
                Bytes::from_static(b"\0\0\0\0\x05again"),
            ] {
                stream.send_data(body, false)?;
                tokio::task::yield_now().await;
            }
            stream.send_trailers(grpc_trailers("0", "streamed", "stream"))?;
        }
        "/grpc/upload" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            stream.send_data(Bytes::from_static(GRPC_BODY), false)?;
            stream.send_trailers(grpc_trailers("0", "uploaded", "upload"))?;
        }
        "/grpc/large" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            for _ in 0..256 {
                stream.send_data(Bytes::from(vec![b'x'; 16 * 1024]), false)?;
                tokio::task::yield_now().await;
            }
            stream.send_trailers(grpc_trailers("0", "large", "large"))?;
        }
        "/grpc/full" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            stream.send_data(Bytes::from_static(GRPC_BODY), false)?;
            stream.send_trailers(grpc_trailers("0", "completed", "full"))?;
        }
        "/grpc/error" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            stream.send_trailers(grpc_trailers("7", "permission denied", "trailers-only"))?;
        }
        "/grpc/slow" => {
            sleep(Duration::from_millis(250)).await;
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .body(())?;
            let mut stream = respond.send_response(response, false)?;
            stream.send_data(Bytes::from_static(GRPC_BODY), false)?;
            stream.send_trailers(grpc_trailers("0", "slow", "slow"))?;
        }
        "/grpc/reset" => {
            respond.send_reset(h2::Reason::CANCEL);
        }
        "/grpc/malformed" => {
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/grpc")
                .header(header::CONTENT_LENGTH, "5")
                .body(())?;
            respond.send_response(response, true)?;
        }
        _ => {
            let response = Response::builder().status(StatusCode::NOT_FOUND).body(())?;
            respond.send_response(response, true)?;
        }
    }
    Ok(())
}

fn grpc_trailers(status: &'static str, message: &'static str, custom: &'static str) -> HeaderMap {
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", status.parse().expect("gRPC status value"));
    trailers.insert("grpc-message", message.parse().expect("gRPC message value"));
    trailers.insert(
        "x-oxiroute-trailer",
        custom.parse().expect("custom trailer"),
    );
    trailers
}

pub struct H2Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub trailers: Option<HeaderMap>,
}

pub struct H2Client {
    sender: h2::client::SendRequest<Bytes>,
    connection: JoinHandle<Result<(), h2::Error>>,
}

impl H2Client {
    pub async fn from_tls(stream: ClientTlsStream<TcpStream>) -> Result<Self, BoxError> {
        let (sender, connection) = h2::client::handshake(stream).await?;
        let connection = tokio::spawn(connection);
        Ok(Self { sender, connection })
    }

    pub async fn request(&mut self, method: Method, path: &str) -> Result<H2Response, BoxError> {
        self.request_inner(method, path, None).await
    }

    pub async fn request_with_body_chunks(
        &mut self,
        method: Method,
        path: &str,
        body: Vec<Bytes>,
    ) -> Result<H2Response, BoxError> {
        self.request_inner(method, path, Some(body)).await
    }

    pub async fn request_with_headers_and_body_chunks(
        &mut self,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: Vec<Bytes>,
    ) -> Result<H2Response, BoxError> {
        self.request_inner_with_headers(method, path, headers, Some(body))
            .await
    }

    pub async fn cancel_request(&mut self, method: Method, path: &str) -> Result<(), BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let request = Request::builder()
            .method(method)
            .uri(format!("https://{PROXY_SERVER_NAME}{path}"))
            .body(())?;
        let (response, mut request_body) = sender.send_request(request, false)?;
        request_body.send_reset(h2::Reason::CANCEL);
        match timeout(IO_TIMEOUT, response).await {
            Ok(Ok(_)) => Err(io::Error::other("H2 request reset was not observed").into()),
            Ok(Err(_)) | Err(_) => Ok(()),
        }
    }

    async fn request_inner(
        &mut self,
        method: Method,
        path: &str,
        body: Option<Vec<Bytes>>,
    ) -> Result<H2Response, BoxError> {
        self.request_inner_with_headers(method, path, HeaderMap::new(), body)
            .await
    }

    async fn request_inner_with_headers(
        &mut self,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: Option<Vec<Bytes>>,
    ) -> Result<H2Response, BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let mut request = Request::builder()
            .method(method)
            .uri(format!("https://{PROXY_SERVER_NAME}{path}"))
            .body(())?;
        *request.headers_mut() = headers;
        let (response, mut request_body) = sender.send_request(request, body.is_none())?;
        if let Some(body) = body {
            let chunk_count = body.len();
            for (index, chunk) in body.into_iter().enumerate() {
                request_body.reserve_capacity(chunk.len());
                while request_body.capacity() < chunk.len() {
                    match futures_util::future::poll_fn(|cx| request_body.poll_capacity(cx)).await {
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error.into()),
                        None => {
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "H2 request body stream closed",
                            )
                            .into());
                        }
                    }
                }
                request_body.send_data(chunk, index + 1 == chunk_count)?;
            }
        }
        let response = timeout(IO_TIMEOUT, response)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "H2 response timed out"))??;
        let status = response.status();
        let headers = response.headers().clone();
        let mut stream = response.into_body();
        let mut body = BytesMut::new();
        while let Some(chunk) = stream.data().await {
            let chunk = chunk?;
            body.extend_from_slice(&chunk);
            stream.flow_control().release_capacity(chunk.len())?;
        }
        let trailers = stream.trailers().await?;
        Ok(H2Response {
            status,
            headers,
            body: body.freeze(),
            trailers,
        })
    }

    pub async fn finish(self) {
        drop(self.sender);
        self.connection.abort();
        let _ = self.connection.await;
    }
}

fn tls_server_config(alpn: &[Vec<u8>]) -> ServerConfig {
    tls_server_config_with_versions(alpn, &[&rustls::version::TLS13, &rustls::version::TLS12])
}

fn tls_server_config_with_versions(
    alpn: &[Vec<u8>],
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> ServerConfig {
    let private_key = private_key_fixture("origin-key.pem");
    tls_server_config_with_identity_and_versions(
        alpn,
        versions,
        &fixture("origin.pem"),
        private_key.path(),
    )
}

fn tls_server_config_with_identity(
    alpn: &[Vec<u8>],
    certificate_chain_path: &Path,
    private_key_path: &Path,
) -> ServerConfig {
    tls_server_config_with_identity_and_versions(
        alpn,
        &[&rustls::version::TLS13, &rustls::version::TLS12],
        certificate_chain_path,
        private_key_path,
    )
}

fn tls_server_config_with_identity_and_versions(
    alpn: &[Vec<u8>],
    versions: &[&'static rustls::SupportedProtocolVersion],
    certificate_chain_path: &Path,
    private_key_path: &Path,
) -> ServerConfig {
    let certificates = load_certificates(certificate_chain_path).expect("origin certificates");
    let private_key = load_private_key(private_key_path).expect("origin private key");
    let mut config = ServerConfig::builder_with_protocol_versions(versions)
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .expect("origin TLS identity");
    config.alpn_protocols = alpn.to_vec();
    config
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, BoxError> {
    let mut reader = BufReader::new(File::open(path)?);
    CertificateDer::pem_reader_iter(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, BoxError> {
    let mut reader = BufReader::new(File::open(path)?);
    PrivateKeyDer::pem_reader_iter(&mut reader)
        .next()
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "fixture has no private key").into()
        })
}

async fn wait_for_counter(counter: &AtomicUsize, expected: usize, label: &str) {
    timeout(IO_TIMEOUT, async {
        while counter.load(Ordering::SeqCst) < expected {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} did not reach {expected}"));
}

async fn finish_origin_task(mut task: JoinHandle<Result<(), BoxError>>) {
    if let Ok(result) = timeout(IO_TIMEOUT, &mut task).await {
        result.expect("origin task").expect("origin service");
    } else {
        task.abort();
        let _ = task.await;
        panic!("origin did not stop within {IO_TIMEOUT:?}");
    }
}
