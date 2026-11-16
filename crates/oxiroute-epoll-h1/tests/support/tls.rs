use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

use oxiroute_epoll_h1::{Timeouts, TlsRouter};
use rcgen::generate_simple_self_signed;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, StreamOwned,
    SupportedProtocolVersion,
    crypto::ring,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
    server::{ClientHello, ResolvesServerCertUsingSni, WebPkiClientVerifier},
    sign::CertifiedKey,
};

use super::{Fixture, Wire};

pub struct Identity {
    pub name: String,
    pub cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Identity {
    pub fn new(name: &str) -> Self {
        let generated = generate_simple_self_signed(vec![name.to_owned()]).expect("certificate");
        Self {
            name: name.to_owned(),
            cert: generated.cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()),
        }
    }

    fn signed(&self) -> Arc<CertifiedKey> {
        let key = ring::sign::any_supported_type(&self.key.clone_key().into()).expect("key");
        Arc::new(CertifiedKey::new(vec![self.cert.clone()], key))
    }
}

pub struct Settings {
    pub names: Vec<&'static str>,
    pub versions: &'static [&'static SupportedProtocolVersion],
    pub alpn: Vec<&'static [u8]>,
    pub client_ca: Option<CertificateDer<'static>>,
    pub tickets: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            names: vec!["a.example"],
            versions: rustls::ALL_VERSIONS,
            alpn: vec![b"http/1.1"],
            client_ca: None,
            tickets: false,
        }
    }
}

pub struct Server {
    config: Arc<ServerConfig>,
    pub identities: Vec<Identity>,
}

impl Server {
    pub fn new(settings: &Settings) -> Arc<Self> {
        let identities = settings
            .names
            .iter()
            .map(|name| Identity::new(name))
            .collect::<Vec<_>>();
        let mut resolver = ResolvesServerCertUsingSni::new();
        for identity in &identities {
            resolver
                .add(&identity.name, identity.signed().as_ref().clone())
                .expect("a name the certificate covers");
        }
        let builder = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(settings.versions)
            .expect("versions");
        let builder = match &settings.client_ca {
            Some(ca) => {
                let mut roots = RootCertStore::empty();
                roots.add(ca.clone()).expect("client root");
                builder.with_client_cert_verifier(
                    WebPkiClientVerifier::builder_with_provider(
                        Arc::new(roots),
                        Arc::new(ring::default_provider()),
                    )
                    .build()
                    .expect("verifier"),
                )
            }
            None => builder.with_no_client_auth(),
        };
        let mut config = builder.with_cert_resolver(Arc::new(resolver));
        config.alpn_protocols = settings.alpn.iter().map(|alpn| alpn.to_vec()).collect();
        if settings.tickets {
            config.ticketer = ring::Ticketer::new().expect("ticketer");
        }
        Arc::new(Self {
            config: Arc::new(config),
            identities,
        })
    }

    fn trust(&self) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        for identity in &self.identities {
            roots.add(identity.cert.clone()).expect("root");
        }
        roots
    }
}

impl TlsRouter for Server {
    fn config(&self, _hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>> {
        Some(Arc::clone(&self.config))
    }
}

pub struct ClientSettings {
    pub versions: &'static [&'static SupportedProtocolVersion],
    pub alpn: Vec<&'static [u8]>,
    pub identity: Option<Identity>,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            versions: rustls::ALL_VERSIONS,
            alpn: vec![b"http/1.1"],
            identity: None,
        }
    }
}

pub struct Trickle(pub TcpStream);

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Trickle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(&buf[..buf.len().min(1)])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub type Tls = StreamOwned<ClientConnection, TcpStream>;

pub struct Client {
    config: Arc<ClientConfig>,
    addr: SocketAddr,
}

impl Client {
    pub fn new(addr: SocketAddr, server: &Server, settings: ClientSettings) -> Self {
        let builder = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(settings.versions)
            .expect("versions")
            .with_root_certificates(server.trust());
        let mut config = match settings.identity {
            Some(identity) => builder
                .with_client_auth_cert(vec![identity.cert.clone()], identity.key.clone_key().into())
                .expect("client identity"),
            None => builder.with_no_client_auth(),
        };
        config.alpn_protocols = settings.alpn.iter().map(|alpn| alpn.to_vec()).collect();
        Self {
            config: Arc::new(config),
            addr,
        }
    }

    fn socket(&self) -> TcpStream {
        let socket = TcpStream::connect(self.addr).expect("connect to the engine");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        socket.set_nodelay(true).expect("nodelay");
        socket
    }

    fn session(&self, name: &str) -> ClientConnection {
        let name = ServerName::try_from(name.to_owned()).expect("server name");
        ClientConnection::new(Arc::clone(&self.config), name).expect("client connection")
    }

    pub fn connect(&self, name: &str) -> io::Result<Wire<Tls>> {
        let mut tls = StreamOwned::new(self.session(name), self.socket());
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock)?;
        }
        Ok(Wire::over(tls))
    }

    pub fn trickle(&self, name: &str) -> io::Result<Wire<StreamOwned<ClientConnection, Trickle>>> {
        let mut tls = StreamOwned::new(self.session(name), Trickle(self.socket()));
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock)?;
        }
        Ok(Wire::over(tls))
    }
}

pub struct Rig {
    pub fixture: Fixture,
    pub client: Client,
    pub server: Arc<Server>,
}

impl Rig {
    pub fn start(settings: &Settings, client: ClientSettings) -> Self {
        Self::with_timeouts(settings, client, Timeouts::default())
    }

    pub fn with_timeouts(settings: &Settings, client: ClientSettings, timeouts: Timeouts) -> Self {
        let server = Server::new(settings);
        let fixture = Fixture::terminating(Arc::clone(&server), timeouts);
        let client = Client::new(fixture.addr, &server, client);
        Self {
            fixture,
            client,
            server,
        }
    }

    pub fn wire(&self) -> Wire<Tls> {
        self.client.connect("a.example").expect("a TLS connection")
    }
}
