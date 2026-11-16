use std::path::PathBuf;

use super::{MAX_SAFE_INTEGER, socket};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, DownstreamTimeoutPolicy, KtlsMode, Listener,
    ListenerBind, Protocol, TlsClientAuthMode, TlsClientAuthPolicy, TlsPolicy, TlsProfile,
    TlsSessionCache, TlsVersion,
};

pub(crate) fn test_certificates() -> Vec<Certificate> {
    vec![
        Certificate {
            name: "files-certificate".into(),
            dns_names: vec!["FILES.EXAMPLE.TEST".into(), "*.FILES.EXAMPLE.TEST".into()],
            source: CertificateSource::Files {
                certificate_chain_path: PathBuf::from("/etc/oxiroute/files-\"chain\".pem"),
                private_key_path: PathBuf::from("/etc/oxiroute/files-\\key.pem"),
            },
        },
        Certificate {
            name: "certbot-certificate".into(),
            dns_names: vec!["CERTBOT.EXAMPLE.TEST".into()],
            source: CertificateSource::Certbot {
                live_directory_path: PathBuf::from("/etc/letsencrypt/live/certbot-é.example.test"),
                archive_directory_path: PathBuf::from(
                    "/etc/letsencrypt/archive/certbot-é.example.test",
                ),
            },
        },
    ]
}

pub(crate) fn test_tls_profiles() -> Vec<TlsProfile> {
    vec![
        TlsProfile {
            name: "edge-tls".into(),
            certificates: vec!["files-certificate".into(), "certbot-certificate".into()],
            default_certificate: "certbot-certificate".into(),
            min_version: TlsVersion::Tls13,
            alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
            policy: TlsPolicy {
                cipher_list: Some("ECDHE-ECDSA-AES128-GCM-SHA256".into()),
                dh_parameters_path: Some("/etc/oxiroute/dhparam.pem".into()),
                client_auth: TlsClientAuthPolicy {
                    mode: TlsClientAuthMode::Optional,
                    ca_certificate_path: Some("/etc/oxiroute/client-ca.pem".into()),
                    allowed_dns_names: vec!["CLIENT.EXAMPLE.TEST".into()],
                },
                session_cache: Some(TlsSessionCache {
                    name: "le_nginx_SSL".into(),
                    size_bytes: 10 * 1024 * 1024,
                }),
                session_timeout_seconds: Some(86_400),
                session_tickets: false,
                prefer_server_ciphers: false,
                backend: Some(oxiroute_config::TlsBackend::Openssl),
                key_exchange_groups: None,
                ktls: KtlsMode::Auto,
            },
        },
        TlsProfile {
            name: "files-tls".into(),
            certificates: vec!["files-certificate".into()],
            default_certificate: "files-certificate".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2],
            policy: TlsPolicy::default(),
        },
    ]
}

pub(crate) fn test_listeners() -> Vec<Listener> {
    vec![
        Listener {
            name: "secure-web".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:8443"),
                ipv6_only: false,
            },
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: Some("edge-tls".into()),
            proxy_protocol: None,
            max_connections: Some(MAX_SAFE_INTEGER),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        },
        Listener {
            name: "plain-web".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:8080"),
                ipv6_only: false,
            },
            protocol: Protocol::Http,
            service: Some("fallback".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(10_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        },
        Listener {
            name: "database".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:15432"),
                ipv6_only: false,
            },
            protocol: Protocol::Tcp,
            service: Some("database".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(2_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        },
        Listener {
            name: "live".into(),
            bind: ListenerBind::Unix {
                path: "/run//oxiroute/live.sock".into(),
                mode: Some(0o660),
            },
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        },
    ]
}
