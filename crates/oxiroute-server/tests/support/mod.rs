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
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
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
pub mod h3;

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

include!("certificates.rs");
include!("proxy.rs");
include!("tls.rs");
include!("h1.rs");
include!("h2.rs");
