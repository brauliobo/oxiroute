use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File},
    io::{self, BufReader},
    net::{Ipv4Addr, SocketAddr},
    os::fd::IntoRawFd,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    ssl::{
        HandshakeError, SslAcceptor, SslConnector, SslFiletype, SslMethod, SslOptions,
        SslVerifyMode, SslVersion,
    },
    x509::{
        X509, X509Name, X509NameBuilder,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, HttpPathSelector, HttpProxyPolicy,
    HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy, Listener, ListenerBind, Protocol,
    TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamPool, UpstreamTls,
};
use oxiroute_server::{
    ActiveCertificateGeneration, CertbotReconciler, CertbotWatcherConfig, CertbotWatcherSupervisor,
    HttpListenerApp, HttpReverseProxy, MAX_HTTP_ATTEMPTS, MonitoredHttpApp, RuntimeMetrics,
    ServiceKind, TlsAlpnChallengeStore, runtime_plan,
};
use pingora::{
    proxy::http_proxy,
    server::{Fds, configuration::ServerConf},
    services::{Service as PingoraService, listening::Service as ListeningService},
};
use rustls::{
    ClientConfig, HandshakeKind, ProtocolVersion, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex as TokioMutex, watch},
    task::{JoinHandle, JoinSet, spawn_blocking},
    time::{Instant, sleep, timeout},
};
use tokio_rustls::{
    TlsAcceptor, TlsConnector, client::TlsStream as ClientTlsStream,
    server::TlsStream as ServerTlsStream,
};

mod config;
mod fixtures;

pub use config::{empty_config, socket_bind, socket_endpoint};
use fixtures::PrivateKeyFixture;
pub use fixtures::{
    certificate_chain_fixture, copy_private_key_fixture, fixture, private_key_fixture,
};

pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(10);
const CONNECT_RETRY_BUDGET: Duration = Duration::from_secs(1);

pub const PROXY_SERVER_NAME: &str = "proxy.example.test";
pub const ORIGIN_SERVER_NAME: &str = "origin.example.test";
pub const GRPC_BODY: &[u8] = b"\0\0\0\0\x05hello";

type BoxError = Box<dyn Error + Send + Sync>;

pub struct TestOnlyEcdsaChain {
    pub fullchain_path: PathBuf,
    pub leaf_private_key_path: PathBuf,
    pub root_certificate_path: PathBuf,
    pub intermediate_certificate_path: PathBuf,
    pub leaf_der: Vec<u8>,
    _directory: TempDir,
}

pub struct TestCertbotLineage {
    name: String,
    pub live_directory_path: PathBuf,
    pub archive_directory_path: PathBuf,
    _directory: TempDir,
}

impl TestCertbotLineage {
    pub fn new(name: &str, initial: &TestOnlyEcdsaChain) -> Self {
        let directory = TempDir::new().expect("test Certbot lineage directory");
        let live_directory_path = directory.path().join("live").join(name);
        let archive_directory_path = directory.path().join("archive").join(name);
        fs::create_dir_all(&live_directory_path).expect("create test Certbot live directory");
        fs::create_dir_all(&archive_directory_path).expect("create test Certbot archive directory");
        let lineage = Self {
            name: name.into(),
            live_directory_path,
            archive_directory_path,
            _directory: directory,
        };
        lineage.write_revision(1, initial);
        lineage.activate(1);
        lineage
    }

    pub fn source(&self) -> CertificateSource {
        CertificateSource::Certbot {
            live_directory_path: self.live_directory_path.clone(),
            archive_directory_path: self.archive_directory_path.clone(),
        }
    }

    pub fn write_revision(&self, revision: u64, material: &TestOnlyEcdsaChain) {
        use std::os::unix::fs::PermissionsExt as _;

        let fullchain = fs::read(&material.fullchain_path).expect("read Certbot test fullchain");
        let certificates = X509::stack_from_pem(&fullchain).expect("parse Certbot test fullchain");
        let cert = certificates[0].to_pem().expect("encode Certbot test leaf");
        let chain = certificates[1..]
            .iter()
            .flat_map(|certificate| certificate.to_pem().expect("encode Certbot test issuer"))
            .collect::<Vec<_>>();
        fs::write(
            self.archive_directory_path
                .join(format!("cert{revision}.pem")),
            cert,
        )
        .expect("write Certbot test leaf");
        fs::write(
            self.archive_directory_path
                .join(format!("chain{revision}.pem")),
            chain,
        )
        .expect("write Certbot test chain");
        fs::write(
            self.archive_directory_path
                .join(format!("fullchain{revision}.pem")),
            fullchain,
        )
        .expect("write Certbot test fullchain");
        let key_path = self
            .archive_directory_path
            .join(format!("privkey{revision}.pem"));
        fs::copy(&material.leaf_private_key_path, &key_path)
            .expect("write Certbot test private key");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .expect("secure Certbot test private key");
    }

    pub fn activate(&self, revision: u64) {
        use std::os::unix::fs::symlink;

        for stem in ["cert", "chain", "fullchain", "privkey"] {
            let link = self.live_directory_path.join(format!("{stem}.pem"));
            if fs::symlink_metadata(&link).is_ok() {
                fs::remove_file(&link).expect("remove prior Certbot test link");
            }
            symlink(
                Path::new("../../archive")
                    .join(&self.name)
                    .join(format!("{stem}{revision}.pem")),
                link,
            )
            .expect("write Certbot test link");
        }
    }
}

pub fn generate_test_only_ecdsa_chain(server_name: &str) -> TestOnlyEcdsaChain {
    generate_test_only_chain(server_name, false)
}

pub fn generate_test_only_client_chain(client_name: &str) -> TestOnlyEcdsaChain {
    generate_test_only_chain(client_name, true)
}

fn generate_test_only_chain(leaf_name: &str, client_auth: bool) -> TestOnlyEcdsaChain {
    let directory = TempDir::new().expect("test-only ECDSA fixture directory");
    let root_key = test_only_ec_key();
    let root_name = test_only_name("OxiRoute Wire Test-Only ECDSA Root");
    let root = test_only_root(&root_key, &root_name);
    let intermediate_key = test_only_ec_key();
    let intermediate_name = test_only_name("OxiRoute Wire Test-Only ECDSA Intermediate");
    let intermediate =
        test_only_intermediate(&intermediate_key, &intermediate_name, &root, &root_key);
    let leaf_key = test_only_ec_key();
    let leaf = test_only_leaf(
        leaf_name,
        &leaf_key,
        &intermediate,
        &intermediate_key,
        client_auth,
    );

    let fullchain_path = directory
        .path()
        .join("wire-test-only-ecdsa-leaf-fullchain.pem");
    let leaf_private_key_path = directory
        .path()
        .join("wire-test-only-ecdsa-leaf-private-key.pem");
    let root_certificate_path = directory
        .path()
        .join("wire-test-only-ecdsa-root-certificate.pem");
    let intermediate_certificate_path = directory
        .path()
        .join("wire-test-only-ecdsa-intermediate-certificate.pem");
    let mut fullchain = leaf.to_pem().expect("test-only ECDSA leaf PEM");
    fullchain.extend_from_slice(
        &intermediate
            .to_pem()
            .expect("test-only ECDSA intermediate PEM"),
    );
    fs::write(&fullchain_path, fullchain).expect("write test-only ECDSA fullchain");
    fs::write(
        &leaf_private_key_path,
        leaf_key
            .private_key_to_pem_pkcs8()
            .expect("test-only ECDSA leaf private key PEM"),
    )
    .expect("write test-only ECDSA leaf private key");
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&leaf_private_key_path, fs::Permissions::from_mode(0o600))
            .expect("secure test-only ECDSA private key");
    }
    fs::write(
        &root_certificate_path,
        root.to_pem().expect("test-only ECDSA root PEM"),
    )
    .expect("write test-only ECDSA root certificate");
    fs::write(
        &intermediate_certificate_path,
        intermediate
            .to_pem()
            .expect("test-only ECDSA intermediate PEM"),
    )
    .expect("write test-only ECDSA intermediate certificate");

    TestOnlyEcdsaChain {
        fullchain_path,
        leaf_private_key_path,
        root_certificate_path,
        intermediate_certificate_path,
        leaf_der: leaf.to_der().expect("test-only ECDSA leaf DER"),
        _directory: directory,
    }
}

fn test_only_ec_key() -> PKey<Private> {
    let group =
        EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).expect("test-only ECDSA P-256 group");
    let key = EcKey::generate(&group).expect("test-only ECDSA key");
    PKey::from_ec_key(key).expect("test-only ECDSA PKey")
}

fn test_only_name(common_name: &str) -> X509Name {
    let mut name = X509NameBuilder::new().expect("test-only certificate name");
    name.append_entry_by_text("CN", common_name)
        .expect("test-only certificate common name");
    name.build()
}

fn test_only_serial(value: u32) -> Asn1Integer {
    Asn1Integer::from_bn(&BigNum::from_u32(value).expect("test-only serial number"))
        .expect("test-only ASN.1 serial number")
}

fn test_only_root(key: &PKey<Private>, name: &X509Name) -> X509 {
    let mut certificate = X509::builder().expect("test-only root builder");
    certificate.set_version(2).expect("test-only root version");
    certificate
        .set_serial_number(&test_only_serial(0x4001))
        .expect("test-only root serial");
    certificate
        .set_subject_name(name)
        .expect("test-only root subject");
    certificate
        .set_issuer_name(name)
        .expect("test-only root issuer");
    certificate.set_pubkey(key).expect("test-only root key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .ca()
                .build()
                .expect("test-only root basic constraints"),
        )
        .expect("append test-only root basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .expect("test-only root key usage"),
        )
        .expect("append test-only root key usage");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(None, None))
        .expect("test-only root subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only root subject key identifier");
    certificate
        .sign(key, MessageDigest::sha256())
        .expect("sign test-only root");
    certificate.build()
}

fn test_only_intermediate(
    key: &PKey<Private>,
    name: &X509Name,
    root: &X509,
    root_key: &PKey<Private>,
) -> X509 {
    let mut certificate = X509::builder().expect("test-only intermediate builder");
    certificate
        .set_version(2)
        .expect("test-only intermediate version");
    certificate
        .set_serial_number(&test_only_serial(0x4002))
        .expect("test-only intermediate serial");
    certificate
        .set_subject_name(name)
        .expect("test-only intermediate subject");
    certificate
        .set_issuer_name(root.subject_name())
        .expect("test-only intermediate issuer");
    certificate
        .set_pubkey(key)
        .expect("test-only intermediate key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .ca()
                .pathlen(0)
                .build()
                .expect("test-only intermediate basic constraints"),
        )
        .expect("append test-only intermediate basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .expect("test-only intermediate key usage"),
        )
        .expect("append test-only intermediate key usage");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(Some(root), None))
        .expect("test-only intermediate subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only intermediate subject key identifier");
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .build(&certificate.x509v3_context(Some(root), None))
        .expect("test-only intermediate authority key identifier");
    certificate
        .append_extension(authority_key_identifier)
        .expect("append test-only intermediate authority key identifier");
    certificate
        .sign(root_key, MessageDigest::sha256())
        .expect("sign test-only intermediate");
    certificate.build()
}

fn test_only_leaf(
    leaf_name: &str,
    key: &PKey<Private>,
    intermediate: &X509,
    intermediate_key: &PKey<Private>,
    client_auth: bool,
) -> X509 {
    let name = test_only_name(leaf_name);
    let mut certificate = X509::builder().expect("test-only ECDSA leaf builder");
    certificate
        .set_version(2)
        .expect("test-only ECDSA leaf version");
    certificate
        .set_serial_number(&test_only_serial(0x4003))
        .expect("test-only ECDSA leaf serial");
    certificate
        .set_subject_name(&name)
        .expect("test-only ECDSA leaf subject");
    certificate
        .set_issuer_name(intermediate.subject_name())
        .expect("test-only ECDSA leaf issuer");
    certificate
        .set_pubkey(key)
        .expect("test-only ECDSA leaf key");
    set_test_only_validity(&mut certificate);
    certificate
        .append_extension(
            BasicConstraints::new()
                .critical()
                .build()
                .expect("test-only ECDSA leaf basic constraints"),
        )
        .expect("append test-only ECDSA leaf basic constraints");
    certificate
        .append_extension(
            KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .expect("test-only ECDSA leaf key usage"),
        )
        .expect("append test-only ECDSA leaf key usage");
    let mut extended_key_usage = ExtendedKeyUsage::new();
    if client_auth {
        extended_key_usage.client_auth();
    } else {
        extended_key_usage.server_auth();
    }
    certificate
        .append_extension(
            extended_key_usage
                .build()
                .expect("test-only ECDSA leaf extended key usage"),
        )
        .expect("append test-only ECDSA leaf extended key usage");
    let subject_alternative_name = SubjectAlternativeName::new()
        .dns(leaf_name)
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf SAN");
    certificate
        .append_extension(subject_alternative_name)
        .expect("append test-only ECDSA leaf SAN");
    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf subject key identifier");
    certificate
        .append_extension(subject_key_identifier)
        .expect("append test-only ECDSA leaf subject key identifier");
    let authority_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(true)
        .build(&certificate.x509v3_context(Some(intermediate), None))
        .expect("test-only ECDSA leaf authority key identifier");
    certificate
        .append_extension(authority_key_identifier)
        .expect("append test-only ECDSA leaf authority key identifier");
    certificate
        .sign(intermediate_key, MessageDigest::sha256())
        .expect("sign test-only ECDSA leaf");
    certificate.build()
}

fn set_test_only_validity(certificate: &mut openssl::x509::X509Builder) {
    certificate
        .set_not_before(&Asn1Time::days_from_now(0).expect("test-only not before"))
        .expect("set test-only not before");
    certificate
        .set_not_after(&Asn1Time::days_from_now(30).expect("test-only not after"))
        .expect("set test-only not after");
}

pub struct ReservedListener {
    pub address: SocketAddr,
    listener: std::net::TcpListener,
}

impl ReservedListener {
    pub fn new() -> Self {
        let listener =
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve proxy listener");
        listener
            .set_nonblocking(true)
            .expect("make proxy listener nonblocking");
        let address = listener.local_addr().expect("reserved proxy address");
        Self { address, listener }
    }
}

pub struct ProxyConfig {
    config: Config,
    _directory: TempDir,
}

impl std::ops::Deref for ProxyConfig {
    type Target = Config;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl std::ops::DerefMut for ProxyConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config
    }
}

pub fn proxy_config(
    listener_address: SocketAddr,
    origin_address: SocketAddr,
    downstream_alpn: Vec<AlpnProtocol>,
    upstream_tls: Option<UpstreamTls>,
    upstream_versions: HttpVersionPolicy,
) -> ProxyConfig {
    let directory = TempDir::new().expect("proxy config private-key directory");
    let private_key_path = copy_private_key_fixture(directory.path(), "proxy-a-key.pem");
    let config = Config {
        certificates: vec![Certificate {
            name: "downstream".into(),
            dns_names: vec![PROXY_SERVER_NAME.into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture("proxy-a.pem"),
                private_key_path,
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "downstream".into(),
            certificates: vec!["downstream".into()],
            default_certificate: "downstream".into(),
            min_version: TlsVersion::Tls12,
            alpn: downstream_alpn,
            policy: oxiroute_config::TlsPolicy::default(),
        }],
        listeners: vec![Listener {
            name: "wire".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Http,
            service: Some("wire".into()),
            tls_profile: Some("downstream".into()),
            proxy_protocol: None,
            max_connections: Some(100),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(origin_address)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: upstream_tls,
            http_versions: upstream_versions,
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        }],
        http_services: vec![HttpService {
            name: "wire".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy::default(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 2_000,
            max_request_body_bytes: Some(64 * 1024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    };
    ProxyConfig {
        config,
        _directory: directory,
    }
}

pub fn verified_upstream(server_name: &str, ca_fixture: &str) -> UpstreamTls {
    UpstreamTls {
        server_name: server_name.into(),
        ca_certificate_path: Some(fixture(ca_fixture)),
    }
}

pub struct ProxyHarness {
    pub address: SocketAddr,
    pub active_certificate: Arc<ActiveCertificateGeneration>,
    pub tls_alpn_challenges: TlsAlpnChallengeStore,
    active_certificates: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    certbot_reconcilers: BTreeMap<String, Arc<CertbotReconciler>>,
    certbot_watcher: Option<CertbotWatcherSupervisor>,
    runtime_metrics: RuntimeMetrics,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ProxyHarness {
    pub fn start(config: &Config, reserved: ReservedListener) -> Self {
        let mut plan = runtime_plan(config).expect("wire runtime plan");
        let certbot_reconcilers = plan
            .certbot_reconcilers()
            .iter()
            .map(|reconciler| (reconciler.status().certificate, Arc::clone(reconciler)))
            .collect::<BTreeMap<_, _>>();
        let certbot_watcher = plan
            .tls
            .start_certbot_watcher(CertbotWatcherConfig::default())
            .expect("wire Certbot watcher");
        let active_certificates = plan.tls.certificates().clone();
        let tls_alpn_challenges = plan.tls.tls_alpn_challenge_store().clone();
        let active_certificate = Arc::clone(
            active_certificates
                .get("downstream")
                .expect("downstream certificate generation"),
        );
        let spec = plan.services.remove(0);
        assert_eq!(
            spec.bind,
            ListenerBind::Socket {
                address: reserved.address,
            }
        );
        let bind = spec.bind.to_string();
        let metrics = RuntimeMetrics::new();
        metrics
            .register_certbot_monitoring(
                certbot_reconcilers.values().cloned(),
                certbot_watcher
                    .as_ref()
                    .map(CertbotWatcherSupervisor::monitor),
            )
            .expect("wire Certbot monitoring");
        let listener_metrics = metrics
            .register_listener(
                &spec.name,
                spec.kind.protocol(),
                bind.clone(),
                spec.max_connections.unwrap_or(u64::MAX),
            )
            .expect("wire listener metrics");
        let ServiceKind::Http(http_service) = spec.kind else {
            panic!("wire listener must compile as HTTP");
        };
        let server_configuration = Arc::new(ServerConf {
            max_retries: MAX_HTTP_ATTEMPTS,
            ..ServerConf::default()
        });
        let app = HttpListenerApp::new(
            http_proxy(
                &server_configuration,
                HttpReverseProxy::new(http_service, listener_metrics.clone()),
            ),
            spec.tls.as_deref(),
        );
        let tls = spec.tls.expect("downstream TLS profile");
        let app = MonitoredHttpApp::new(app, listener_metrics);
        let mut service = ListeningService::new("OxiRoute wire test".into(), app);
        service.add_tls_with_settings(
            &bind,
            None,
            tls.tls_settings().expect("downstream TLS settings"),
        );

        let mut inherited = Fds::new();
        inherited.add(bind, reserved.listener.into_raw_fd());
        let inherited = Arc::new(TokioMutex::new(inherited));
        let (shutdown, shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            PingoraService::start_service(&mut service, Some(inherited), shutdown_watch, 1).await;
        });

        Self {
            address: reserved.address,
            active_certificate,
            tls_alpn_challenges,
            active_certificates,
            certbot_reconcilers,
            certbot_watcher,
            runtime_metrics: metrics,
            shutdown,
            task: Some(task),
        }
    }

    pub fn certificate(&self, name: &str) -> &Arc<ActiveCertificateGeneration> {
        self.active_certificates
            .get(name)
            .unwrap_or_else(|| panic!("missing active certificate `{name}`"))
    }

    pub fn certbot_reconciler(&self, name: &str) -> &Arc<CertbotReconciler> {
        self.certbot_reconcilers
            .get(name)
            .unwrap_or_else(|| panic!("missing Certbot reconciler `{name}`"))
    }

    pub async fn wait_for_active_connections(&self, expected: u64) {
        timeout(IO_TIMEOUT, async {
            loop {
                let snapshot = self
                    .runtime_metrics
                    .snapshot()
                    .expect("wire runtime metrics snapshot");
                if snapshot.traffic.active_connections == expected {
                    break;
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("wire active connections did not reach {expected}"));
    }

    pub async fn finish(mut self) {
        self.shutdown.send(true).expect("signal proxy shutdown");
        let mut task = self.task.take().expect("proxy service task");
        if let Ok(result) = timeout(IO_TIMEOUT, &mut task).await {
            result.expect("proxy service task");
        } else {
            task.abort();
            let _ = task.await;
            panic!("proxy service did not stop within {IO_TIMEOUT:?}");
        }
        if let Some(watcher) = &mut self.certbot_watcher {
            watcher.shutdown();
        }
    }
}

impl Drop for ProxyHarness {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

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

pub struct RawHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub async fn h1_request<S>(stream: &mut S, path: &str, close: bool) -> io::Result<RawHttpResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connection = if close { "close" } else { "keep-alive" };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {PROXY_SERVER_NAME}\r\nConnection: {connection}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut response = read_until_headers(stream).await?;
    let header_end = find_header_end(&response).expect("complete response headers");
    let head = std::str::from_utf8(&response[..header_end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status"))?;
    let content_length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let body_start = header_end + 4;
    let response_end = body_start + content_length;
    if response.len() < response_end {
        let received = response.len();
        response.resize(response_end, 0);
        stream.read_exact(&mut response[received..]).await?;
    } else {
        response.truncate(response_end);
    }
    Ok(RawHttpResponse {
        status,
        body: response[body_start..].to_vec(),
    })
}

async fn read_until_headers<S>(stream: &mut S) -> io::Result<Vec<u8>>
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
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

pub struct PlainH1Origin {
    pub address: SocketAddr,
    requests: Arc<AtomicUsize>,
    http_bytes: Arc<AtomicUsize>,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), BoxError>>>,
}

impl PlainH1Origin {
    pub async fn start(body: &'static [u8]) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("plain origin bind");
        let address = listener.local_addr().expect("plain origin address");
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_task = Arc::clone(&requests);
        let http_bytes = Arc::new(AtomicUsize::new(0));
        let http_bytes_for_task = Arc::clone(&http_bytes);
        let (shutdown, mut shutdown_watch) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        let requests = Arc::clone(&requests_for_task);
                        let http_bytes = Arc::clone(&http_bytes_for_task);
                        connections.spawn(serve_plain_h1(stream, body, requests, http_bytes));
                    }
                    changed = shutdown_watch.changed() => {
                        changed?;
                        break;
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.expect("connection task exists")??;
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            Ok(())
        });
        Self {
            address,
            requests,
            http_bytes,
            shutdown,
            task: Some(task),
        }
    }

    pub async fn wait_for_requests(&self, expected: usize) {
        wait_for_counter(&self.requests, expected, "plain origin requests").await;
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    pub fn http_bytes(&self) -> usize {
        self.http_bytes.load(Ordering::SeqCst)
    }

    pub async fn finish(mut self) {
        self.shutdown.send(true).expect("signal plain origin");
        finish_origin_task(self.task.take().expect("plain origin task")).await;
    }
}

impl Drop for PlainH1Origin {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn serve_plain_h1(
    mut stream: TcpStream,
    body: &'static [u8],
    requests: Arc<AtomicUsize>,
    http_bytes: Arc<AtomicUsize>,
) -> Result<(), BoxError> {
    loop {
        match read_until_headers_observed(&mut stream, &http_bytes).await {
            Ok(_) => {
                requests.fetch_add(1, Ordering::SeqCst);
                write_h1_response(&mut stream, body).await?;
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

async fn write_h1_response<S>(stream: &mut S, body: &[u8]) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

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
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, BoxError> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::private_key(&mut reader)?.ok_or_else(|| {
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
