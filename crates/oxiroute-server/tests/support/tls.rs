pub async fn tls_connect(
    address: SocketAddr,
    server_name: &str,
    ca_fixture: &str,
    alpn: &[&[u8]],
) -> Result<ClientTlsStream<TcpStream>, BoxError> {
    let config = tls_client_config(&fixture(ca_fixture), alpn)?;
    tls_connect_with_config(address, server_name, config).await
}

pub fn tls_client_config(
    ca_certificate_path: &Path,
    alpn: &[&[u8]],
) -> Result<Arc<ClientConfig>, BoxError> {
    tls_client_config_with_versions(
        ca_certificate_path,
        alpn,
        &[&rustls::version::TLS13, &rustls::version::TLS12],
    )
}

pub fn tls_client_config_with_versions(
    ca_certificate_path: &Path,
    alpn: &[&[u8]],
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Result<Arc<ClientConfig>, BoxError> {
    let mut roots = RootCertStore::empty();
    for certificate in load_certificates(ca_certificate_path)? {
        roots.add(certificate)?;
    }
    let mut config = ClientConfig::builder_with_protocol_versions(versions)
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    Ok(Arc::new(config))
}

pub fn tls_client_config_with_identity(
    ca_certificate_path: &Path,
    client_chain_path: &Path,
    client_private_key_path: &Path,
    alpn: &[&[u8]],
) -> Result<Arc<ClientConfig>, BoxError> {
    let mut roots = RootCertStore::empty();
    for certificate in load_certificates(ca_certificate_path)? {
        roots.add(certificate)?;
    }
    let certificates = load_certificates(client_chain_path)?;
    let private_key = load_private_key(client_private_key_path)?;
    let mut config = ClientConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
    .with_root_certificates(roots)
    .with_client_auth_cert(certificates, private_key)?;
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    Ok(Arc::new(config))
}

pub async fn tls_connect_with_config(
    address: SocketAddr,
    server_name: &str,
    config: Arc<ClientConfig>,
) -> Result<ClientTlsStream<TcpStream>, BoxError> {
    let stream = tcp_connect(address).await?;
    tls_handshake(stream, server_name, config).await
}

pub async fn openssl_tls_request_with_identity(
    address: SocketAddr,
    server_name: &str,
    server_ca_path: &Path,
    client_chain_path: &Path,
    client_private_key_path: &Path,
) -> Result<bool, BoxError> {
    let server_name = server_name.to_owned();
    let server_ca_path = server_ca_path.to_owned();
    let client_chain_path = client_chain_path.to_owned();
    let client_private_key_path = client_private_key_path.to_owned();
    timeout(
        IO_TIMEOUT,
        spawn_blocking(move || {
            let mut connector = SslConnector::builder(SslMethod::tls_client())?;
            connector.set_ca_file(server_ca_path)?;
            connector.set_verify(SslVerifyMode::PEER);
            connector.set_certificate_chain_file(client_chain_path)?;
            connector.set_private_key_file(client_private_key_path, SslFiletype::PEM)?;
            let stream = std::net::TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
            stream.set_read_timeout(Some(IO_TIMEOUT))?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            let mut stream = connector.build().connect(&server_name, stream)?;
            std::io::Write::write_all(
                &mut stream,
                b"GET /openssl-client-auth HTTP/1.1\r\nHost: proxy.example.test\r\nConnection: close\r\n\r\n",
            )?;
            let mut response = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut response)?;
            Ok::<_, BoxError>(response.starts_with(b"HTTP/1.1 200"))
        }),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "OpenSSL client request timed out"))?
    .map_err(|error| io::Error::other(format!("OpenSSL client request task failed: {error}")))?
}

pub struct OpenSslTlsHandshake {
    pub peer_leaf: Vec<u8>,
    pub negotiated_alpn: Option<Vec<u8>>,
}

pub async fn openssl_tls_alpn_handshake(
    address: SocketAddr,
    server_name: &str,
    alpn: &[&[u8]],
) -> Result<OpenSslTlsHandshake, Box<dyn Error + Send + Sync>> {
    let server_name = server_name.to_owned();
    let mut alpn_wire = Vec::new();
    for protocol in alpn {
        let length = u8::try_from(protocol.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ALPN protocol too long"))?;
        if length == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty ALPN protocol").into());
        }
        alpn_wire.push(length);
        alpn_wire.extend_from_slice(protocol);
    }
    timeout(
        IO_TIMEOUT,
        spawn_blocking(move || {
            let mut connector = SslConnector::builder(SslMethod::tls_client())?;
            connector.set_verify(SslVerifyMode::NONE);
            connector.set_alpn_protos(&alpn_wire)?;
            let stream = std::net::TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
            stream.set_read_timeout(Some(IO_TIMEOUT))?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            let stream = connector.build().connect(&server_name, stream)?;
            let peer_leaf = stream
                .ssl()
                .peer_certificate()
                .ok_or_else(|| io::Error::other("OpenSSL peer omitted certificate"))?
                .to_der()?;
            let negotiated_alpn = stream.ssl().selected_alpn_protocol().map(ToOwned::to_owned);
            Ok::<_, Box<dyn Error + Send + Sync>>(OpenSslTlsHandshake {
                peer_leaf,
                negotiated_alpn,
            })
        }),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "OpenSSL TLS-ALPN handshake timed out",
        )
    })?
    .map_err(|error| io::Error::other(format!("OpenSSL TLS-ALPN client task failed: {error}")))?
}

pub async fn tls_handshake(
    stream: TcpStream,
    server_name: &str,
    config: Arc<ClientConfig>,
) -> Result<ClientTlsStream<TcpStream>, BoxError> {
    let connector = TlsConnector::from(config);
    let server_name = ServerName::try_from(server_name.to_owned())?;
    let stream = timeout(IO_TIMEOUT, connector.connect(server_name, stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
    Ok(stream)
}

#[derive(Clone, Copy, Debug)]
pub enum LegacyTlsVersion {
    Tls10,
    Tls11,
}

impl LegacyTlsVersion {
    const fn openssl(self) -> SslVersion {
        match self {
            Self::Tls10 => SslVersion::TLS1,
            Self::Tls11 => SslVersion::TLS1_1,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tls10 => "TLSv1",
            Self::Tls11 => "TLSv1.1",
        }
    }
}

pub struct LegacyTlsOrigin {
    pub address: SocketAddr,
    accepted: Arc<AtomicUsize>,
    completed_handshakes: Arc<AtomicUsize>,
    decrypted_bytes: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
    _private_key: PrivateKeyFixture,
}

impl LegacyTlsOrigin {
    pub fn start(version: LegacyTlsVersion) -> Self {
        let private_key = private_key_fixture("origin-key.pem");
        let mut acceptor = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls_server())
            .expect("legacy TLS origin acceptor");
        acceptor.set_security_level(0);
        acceptor.clear_options(SslOptions::NO_TLSV1 | SslOptions::NO_TLSV1_1);
        acceptor
            .set_cipher_list("ECDHE-ECDSA-AES128-SHA:@SECLEVEL=0")
            .expect("legacy TLS origin cipher");
        acceptor
            .set_min_proto_version(Some(version.openssl()))
            .expect("legacy TLS origin minimum version");
        acceptor
            .set_max_proto_version(Some(version.openssl()))
            .expect("legacy TLS origin maximum version");
        acceptor
            .set_certificate_chain_file(fixture("origin.pem"))
            .expect("legacy TLS origin certificate");
        acceptor
            .set_private_key_file(private_key.path(), SslFiletype::PEM)
            .expect("legacy TLS origin private key");
        acceptor
            .check_private_key()
            .expect("legacy TLS origin identity");
        let acceptor = Arc::new(acceptor.build());
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("legacy TLS origin bind");
        listener
            .set_nonblocking(true)
            .expect("legacy TLS origin nonblocking listener");
        let address = listener.local_addr().expect("legacy TLS origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let completed_handshakes = Arc::new(AtomicUsize::new(0));
        let decrypted_bytes = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let task = {
            let accepted = Arc::clone(&accepted);
            let completed_handshakes = Arc::clone(&completed_handshakes);
            let decrypted_bytes = Arc::clone(&decrypted_bytes);
            let shutdown = Arc::clone(&shutdown);
            std::thread::spawn(move || {
                while !shutdown.load(Ordering::SeqCst) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(error) => panic!("legacy TLS origin accept failed: {error}"),
                    };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .expect("legacy TLS origin read timeout");
                    stream
                        .set_write_timeout(Some(IO_TIMEOUT))
                        .expect("legacy TLS origin write timeout");
                    if let Ok(mut stream) = acceptor.accept(stream) {
                        let mut buffer = [0_u8; 1024];
                        if let Ok(count) = io::Read::read(&mut stream, &mut buffer) {
                            decrypted_bytes.fetch_add(count, Ordering::SeqCst);
                        }
                    }
                    completed_handshakes.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        Self {
            address,
            accepted,
            completed_handshakes,
            decrypted_bytes,
            shutdown,
            task: Some(task),
            _private_key: private_key,
        }
    }

    pub async fn wait_for_completed_handshakes(&self, expected: usize) {
        wait_for_counter(
            &self.completed_handshakes,
            expected,
            "legacy TLS origin handshakes",
        )
        .await;
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn decrypted_bytes(&self) -> usize {
        self.decrypted_bytes.load(Ordering::SeqCst)
    }

    pub fn finish(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.task
            .take()
            .expect("legacy TLS origin task")
            .join()
            .expect("legacy TLS origin thread");
    }
}

impl Drop for LegacyTlsOrigin {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

pub async fn direct_legacy_tls_origin_handshake(
    address: SocketAddr,
    version: LegacyTlsVersion,
) -> Result<String, BoxError> {
    timeout(
        IO_TIMEOUT,
        spawn_blocking(move || {
            let mut connector = SslConnector::builder(SslMethod::tls_client())?;
            connector.set_verify(SslVerifyMode::NONE);
            connector.set_security_level(0);
            connector.set_cipher_list("ECDHE-ECDSA-AES128-SHA:@SECLEVEL=0")?;
            connector.set_min_proto_version(Some(version.openssl()))?;
            connector.set_max_proto_version(Some(version.openssl()))?;
            let stream = std::net::TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
            stream.set_read_timeout(Some(IO_TIMEOUT))?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            let stream = connector.build().connect(ORIGIN_SERVER_NAME, stream)?;
            Ok::<_, BoxError>(stream.ssl().version_str().to_owned())
        }),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "legacy origin control timed out"))?
    .map_err(|error| io::Error::other(format!("legacy origin control task failed: {error}")))?
}

pub struct RejectedLegacyTls {
    pub error: String,
    pub server_bytes: Vec<u8>,
}

pub async fn legacy_tls_handshake(
    address: SocketAddr,
    version: LegacyTlsVersion,
) -> Result<RejectedLegacyTls, BoxError> {
    timeout(
        IO_TIMEOUT,
        spawn_blocking(move || legacy_tls_handshake_blocking(address, version)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "legacy TLS client timed out"))?
    .map_err(|error| io::Error::other(format!("legacy TLS client task failed: {error}")))?
}

fn legacy_tls_handshake_blocking(
    address: SocketAddr,
    version: LegacyTlsVersion,
) -> Result<RejectedLegacyTls, BoxError> {
    let mut connector = SslConnector::builder(SslMethod::tls_client())?;
    connector.set_verify(SslVerifyMode::NONE);
    connector.set_security_level(0);
    connector.set_cipher_list("ALL:@SECLEVEL=0")?;
    connector.set_min_proto_version(Some(version.openssl()))?;
    connector.set_max_proto_version(Some(version.openssl()))?;
    let stream = std::net::TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let stream = RecordingTcpStream {
        stream,
        server_bytes: Vec::new(),
    };
    match connector.build().connect(PROXY_SERVER_NAME, stream) {
        Ok(_) => {
            Err(io::Error::other(format!("server accepted legacy TLS version {version:?}")).into())
        }
        Err(HandshakeError::Failure(failure)) => Ok(RejectedLegacyTls {
            error: failure.error().to_string(),
            server_bytes: failure.get_ref().server_bytes.clone(),
        }),
        Err(HandshakeError::SetupFailure(error)) => Err(io::Error::other(format!(
            "legacy TLS client failed before sending a ClientHello: {error}"
        ))
        .into()),
        Err(HandshakeError::WouldBlock(_)) => {
            Err(io::Error::other("blocking legacy TLS client unexpectedly would block").into())
        }
    }
}

struct RecordingTcpStream {
    stream: std::net::TcpStream,
    server_bytes: Vec<u8>,
}

impl io::Read for RecordingTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = io::Read::read(&mut self.stream, buffer)?;
        self.server_bytes.extend_from_slice(&buffer[..count]);
        Ok(count)
    }
}

impl io::Write for RecordingTcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.stream, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.stream)
    }
}

pub async fn tcp_connect(address: SocketAddr) -> io::Result<TcpStream> {
    let deadline = Instant::now() + CONNECT_RETRY_BUDGET;
    loop {
        match timeout(IO_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) if Instant::now() < deadline => {
                let _ = error;
                sleep(CONNECT_RETRY_DELAY).await;
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TCP connect timed out",
                ));
            }
        }
    }
}

pub fn negotiated_tls_is_modern(stream: &ClientTlsStream<TcpStream>) -> bool {
    matches!(
        stream.get_ref().1.protocol_version(),
        Some(ProtocolVersion::TLSv1_2 | ProtocolVersion::TLSv1_3)
    )
}

pub fn negotiated_alpn(stream: &ClientTlsStream<TcpStream>) -> Option<&[u8]> {
    stream.get_ref().1.alpn_protocol()
}

pub fn handshake_kind(stream: &ClientTlsStream<TcpStream>) -> Option<HandshakeKind> {
    stream.get_ref().1.handshake_kind()
}

pub fn peer_certificate_count(stream: &ClientTlsStream<TcpStream>) -> usize {
    stream
        .get_ref()
        .1
        .peer_certificates()
        .map_or(0, <[CertificateDer<'static>]>::len)
}

pub fn peer_leaf(stream: &ClientTlsStream<TcpStream>) -> Vec<u8> {
    stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .expect("peer leaf certificate")
        .as_ref()
        .to_vec()
}

pub fn fixture_leaf(name: &str) -> Vec<u8> {
    load_certificates(&fixture(name))
        .expect("fixture certificate")
        .remove(0)
        .as_ref()
        .to_vec()
}
