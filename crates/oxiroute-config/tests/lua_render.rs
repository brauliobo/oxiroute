use std::{net::SocketAddr, path::PathBuf};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, ConfigError, DnsResolutionPolicy,
    DownstreamTimeoutPolicy, HealthCheck, HealthCheckType, HealthStartup, HttpAccessPolicy,
    HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryBodySafety, HttpRetryMethodSafety, HttpRetryPolicy,
    HttpRetryTarget, HttpRetryTrigger, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpStaticMimePolicy, HttpStaticPathMapping, HttpUpstreamHost, HttpVersion, HttpVersionPolicy,
    L4Service, Listener, ListenerBind, Management, Protocol, RtmpApplication, RtmpRecorder,
    RtmpRecorderSegmentNaming, RtmpRecorderStart, RtmpRecorderTimeBasis, RtmpRecorderTimezone,
    RtmpService, TlsProfile, TlsVersion, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamEndpoint, UpstreamPool, UpstreamServer, UpstreamTls, load_lua, render_lua,
    validate_config,
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
type Mutation = (&'static str, fn(&mut Config));

fn socket(value: &str) -> SocketAddr {
    value.parse().expect("valid test socket address")
}

fn server(name: &str, endpoint: UpstreamEndpoint) -> UpstreamServer {
    UpstreamServer {
        name: name.into(),
        endpoint,
        max_connections: None,
        dns_resolution: DnsResolutionPolicy::OnConnect,
    }
}

fn test_certificates() -> Vec<Certificate> {
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

fn test_tls_profiles() -> Vec<TlsProfile> {
    vec![
        TlsProfile {
            name: "edge-tls".into(),
            certificates: vec!["files-certificate".into(), "certbot-certificate".into()],
            default_certificate: "certbot-certificate".into(),
            min_version: TlsVersion::Tls13,
            alpn: vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        },
        TlsProfile {
            name: "files-tls".into(),
            certificates: vec!["files-certificate".into()],
            default_certificate: "files-certificate".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2],
        },
    ]
}

fn test_listeners() -> Vec<Listener> {
    vec![
        Listener {
            name: "secure-web".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:8443"),
            },
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: Some("edge-tls".into()),
            max_connections: Some(MAX_SAFE_INTEGER),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        },
        Listener {
            name: "plain-web".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:8080"),
            },
            protocol: Protocol::Http,
            service: Some("fallback".into()),
            tls_profile: None,
            max_connections: Some(10_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        },
        Listener {
            name: "database".into(),
            bind: ListenerBind::Socket {
                address: socket("127.0.0.1:15432"),
            },
            protocol: Protocol::Tcp,
            service: Some("database".into()),
            tls_profile: None,
            max_connections: Some(2_000),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
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
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        },
    ]
}

#[allow(clippy::too_many_lines)]
fn test_upstream_pools() -> Vec<UpstreamPool> {
    vec![
        UpstreamPool {
            name: "secure-backends".into(),
            servers: vec![
                server(
                    "secure-ip",
                    UpstreamEndpoint::Socket {
                        address: socket("10.0.0.20:443"),
                    },
                ),
                server(
                    "secure-dns",
                    UpstreamEndpoint::Dns {
                        host: "SECURE.EXAMPLE.TEST".into(),
                        port: 443,
                    },
                ),
            ],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: Some(UpstreamTls {
                server_name: "ORIGIN.EXAMPLE.TEST".into(),
                ca_certificate_path: Some(PathBuf::from("/etc/oxiroute/origin-\"ca\".pem")),
            }),
            http_versions: HttpVersionPolicy {
                min: HttpVersion::Http11,
                max: HttpVersion::Http2,
            },
            queue_timeout_ms: Some(5_000),
            connect_timeout_ms: Some(10_000),
            server_timeout_ms: Some(30_000),
            connection_reuse: UpstreamConnectionReuse::Safe,
        },
        UpstreamPool {
            name: "web-backends".into(),
            servers: vec![
                server(
                    "web-ip",
                    UpstreamEndpoint::Socket {
                        address: socket("127.0.0.1:3001"),
                    },
                ),
                server(
                    "web-dns",
                    UpstreamEndpoint::Dns {
                        host: "WEB.EXAMPLE.TEST".into(),
                        port: 3000,
                    },
                ),
            ],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::LeastConnections,
            health_check: Some(HealthCheck {
                kind: HealthCheckType::Http,
                interval_ms: 5_000,
                timeout_ms: 750,
                healthy_threshold: 2,
                unhealthy_threshold: 4,
                startup: HealthStartup::default(),
                fast_interval_ms: Some(2_000),
                down_interval_ms: Some(20_000),
                host: Some("backend.internal:3000".into()),
                path: Some("/healthz".into()),
                expected_status: Some(200),
                http_version: Some(oxiroute_config::HealthHttpVersion::Http11),
            }),
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Safe,
        },
        UpstreamPool {
            name: "database-backends".into(),
            servers: vec![server(
                "database",
                UpstreamEndpoint::Socket {
                    address: socket("10.0.0.12:5432"),
                },
            )],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: Some(HealthCheck {
                kind: HealthCheckType::Tcp,
                interval_ms: 10_000,
                timeout_ms: 1_000,
                healthy_threshold: 1,
                unhealthy_threshold: 3,
                startup: HealthStartup::default(),
                fast_interval_ms: None,
                down_interval_ms: None,
                host: None,
                path: None,
                expected_status: None,
                http_version: None,
            }),
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Never,
        },
        UpstreamPool {
            name: "unix-backends".into(),
            servers: vec![server(
                "unix",
                UpstreamEndpoint::Unix {
                    path: "/run//oxiroute/backend.sock".into(),
                },
            )],
            endpoints: Vec::new(),
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::Never,
        },
    ]
}

fn test_proxy_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
        upstream_host: HttpUpstreamHost::Endpoint {
            unix_fallback: Some("fallback.internal:443".into()),
        },
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "X-Authority".into(),
                value: HttpRequestHeaderValue::IncomingAuthority,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Literal".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "edge".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Host".into(),
                value: HttpRequestHeaderValue::NormalizedHost,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Client-Ip".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Upstream".into(),
                value: HttpRequestHeaderValue::SelectedUpstreamHost,
            },
            HttpRequestHeaderMutation::Remove {
                name: "X-Remove".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "X-Frame-Options".into(),
                value: "same-origin".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "X-Remove".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/".into(),
            to: "/application".into(),
        }],
        response_cookie_attributes: Vec::new(),
        retry: HttpRetryPolicy {
            max_retries: 2,
            triggers: vec![
                HttpRetryTrigger::ConnectTimeout,
                HttpRetryTrigger::RefusedStream,
            ],
            method_safety: HttpRetryMethodSafety::GetHead,
            body_safety: HttpRetryBodySafety::Empty,
            target: HttpRetryTarget::SameServer,
            delay_ms: 25,
        },
        cache: None,
    }
}

#[allow(clippy::too_many_lines)]
fn test_http_services() -> Vec<HttpService> {
    vec![
        HttpService {
            name: "web".into(),
            routes: vec![
                HttpRoute {
                    host: Some(HttpHostSelector::NormalizedHost {
                        value: "API.EXAMPLE.TEST:443".into(),
                    }),
                    path: HttpPathSelector::SegmentPrefix {
                        value: "/api%3azone/".into(),
                    },
                    methods: vec!["POST".into(), "GET".into()],
                    access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                        token_file_path: "/run/secrets/api-token".into(),
                        header_name: "X-Api-Token".into(),
                        realm: Some("private-api".into()),
                    }),
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::Proxy {
                        upstream_pool: "secure-backends".into(),
                        policy: test_proxy_policy(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/health".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 200,
                        body: "healthy\n".into(),
                        headers: vec![HttpLiteralHeader {
                            name: "X-Source".into(),
                            value: "oxiroute".into(),
                            always: true,
                        }],
                    },
                },
                HttpRoute {
                    host: Some(HttpHostSelector::ExactAuthority {
                        value: "Example.TEST:8443".into(),
                    }),
                    path: HttpPathSelector::RawPrefix {
                        value: "/legacy".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::Redirect {
                        status: 308,
                        location: HttpRedirectLocation::RequestTemplate {
                            value: "$scheme://$host$request_uri".into(),
                            nginx_host_fallback: None,
                        },
                        headers: Vec::new(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::SegmentPrefix {
                        value: "/assets".into(),
                    },
                    methods: vec!["GET".into()],
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::StaticFiles {
                        root_directory: "/srv//www/application".into(),
                        path_mapping: HttpStaticPathMapping::Root,
                        index_files: vec!["index.html".into()],
                        internal_index_redirects: false,
                        directory_redirects: false,
                        spa_fallback: Some("application/index.html".into()),
                        try_files: Vec::new(),
                        autoindex: false,
                        autoindex_exact_size: true,
                        autoindex_local_time: false,
                        mime: HttpStaticMimePolicy::default(),
                        headers: Vec::new(),
                        error_responses: Vec::new(),
                    },
                },
            ],
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(10 * 1024 * 1024),
            gzip: None,
            access_log: None,
        },
        HttpService {
            name: "fallback".into(),
            routes: vec![HttpRoute {
                host: Some(HttpHostSelector::NormalizedHost {
                    value: "*.EXAMPLE.TEST".into(),
                }),
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: vec!["HEAD".into()],
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "web-backends".into(),
                    policy: HttpProxyPolicy {
                        upstream_host: HttpUpstreamHost::Literal {
                            value: "backend.internal".into(),
                        },
                        request_headers: Vec::new(),
                        response_headers: Vec::new(),
                        response_cookie_path_rewrites: Vec::new(),
                        response_cookie_attributes: Vec::new(),
                        retry: HttpRetryPolicy::default(),
                        cache: None,
                    },
                },
            }],
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        },
    ]
}

fn test_l4_services() -> Vec<L4Service> {
    vec![
        L4Service {
            name: "database".into(),
            upstream_pool: "database-backends".into(),
            connect_timeout_ms: MAX_SAFE_INTEGER - 2,
            idle_timeout_ms: MAX_SAFE_INTEGER - 3,
            lifetime_timeout_ms: Some(MAX_SAFE_INTEGER - 4),
        },
        L4Service {
            name: "database-short-lived".into(),
            upstream_pool: "database-backends".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 2_000,
            lifetime_timeout_ms: None,
        },
    ]
}

fn test_rtmp_services() -> Vec<RtmpService> {
    vec![RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        access_log: None,
        applications: vec![RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: true,
            push_targets: Vec::new(),
            fanout: oxiroute_config::RtmpFanoutPolicy {
                max_subscribers: 1_024,
                max_queue_messages_per_subscriber: 256,
                max_queue_bytes_per_subscriber: 8 * 1_024 * 1_024,
            },
            recorders: vec![
                RtmpRecorder {
                    name: "continuous".into(),
                    start: RtmpRecorderStart::Continuous,
                    root_directory: "/var//lib/oxiroute/recordings".into(),
                    suffix_template: "-%Y-%m-%dT%H-%M-%S.flv".into(),
                    append_unix_seconds: true,
                    timezone: RtmpRecorderTimezone::Utc,
                    time_basis: RtmpRecorderTimeBasis::SegmentStart,
                    segment_naming: RtmpRecorderSegmentNaming::SafeUnique,
                    rotation_interval_ms: Some(60_000),
                    max_queue_messages: 256,
                    max_queue_bytes: 8 * 1024 * 1024,
                    shutdown_timeout_ms: 5_000,
                    max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                    max_storage_files: Some(10_000),
                    max_active_recorders: 8,
                },
                RtmpRecorder {
                    name: "manual".into(),
                    start: RtmpRecorderStart::Manual,
                    root_directory: "/var/lib/oxiroute/recordings".into(),
                    suffix_template: ".flv".into(),
                    append_unix_seconds: false,
                    timezone: RtmpRecorderTimezone::Iana("America/Bahia".into()),
                    time_basis: RtmpRecorderTimeBasis::SegmentEnd,
                    segment_naming: RtmpRecorderSegmentNaming::NginxCompatible,
                    rotation_interval_ms: None,
                    max_queue_messages: 64,
                    max_queue_bytes: 1024 * 1024,
                    shutdown_timeout_ms: 3_000,
                    max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                    max_storage_files: Some(10_000),
                    max_active_recorders: 8,
                },
            ],
        }],
    }]
}

fn complete_config() -> Config {
    Config {
        version: 1,
        max_connections: Some(MAX_SAFE_INTEGER),
        management: Some(Management {
            bind: socket("127.0.0.1:9080"),
            ui_dir: Some(PathBuf::from("./ui/dist")),
        }),
        stats: None,
        certificates: test_certificates(),
        tls_profiles: test_tls_profiles(),
        listeners: test_listeners(),
        cache_stores: Vec::new(),
        upstream_pools: test_upstream_pools(),
        http_services: test_http_services(),
        forward_proxy_services: Vec::new(),
        rtmp_services: test_rtmp_services(),
        l4_services: test_l4_services(),
    }
}

fn minimal_config() -> Config {
    Config {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

fn normalized_round_trip(mut config: Config) -> (Config, String) {
    validate_config(&mut config).expect("valid test configuration");
    let source = render_lua(&config).expect("rendered configuration");
    let loaded = load_lua(&source).expect("rendered Lua must load");
    assert_eq!(loaded, config);
    (config, source)
}

const RENDERED_FIELDS: &[&str] = &[
    "version",
    "max_connections",
    "management",
    "certificates",
    "tls_profiles",
    "listeners",
    "cache_stores",
    "upstream_pools",
    "http_services",
    "forward_proxy_services",
    "rtmp_services",
    "l4_services",
    "bind",
    "address",
    "ui_dir",
    "name",
    "dns_names",
    "source",
    "type",
    "certificate_chain_path",
    "private_key_path",
    "live_directory_path",
    "archive_directory_path",
    "default_certificate",
    "min_version",
    "alpn",
    "protocol",
    "service",
    "tls_profile",
    "max_connections",
    "servers",
    "algorithm",
    "health_check",
    "tls",
    "http_versions",
    "server_name",
    "ca_certificate_path",
    "min",
    "max",
    "interval_ms",
    "timeout_ms",
    "healthy_threshold",
    "unhealthy_threshold",
    "host",
    "path",
    "port",
    "routes",
    "upstream_io_timeout_ms",
    "max_request_body_bytes",
    "max_retries",
    "kind",
    "value",
    "access_policy",
    "token_file_path",
    "header_name",
    "realm",
    "action",
    "policy",
    "upstream_host",
    "unix_fallback",
    "request_headers",
    "response_headers",
    "response_cookie_path_rewrites",
    "operation",
    "from",
    "to",
    "retry",
    "triggers",
    "method_safety",
    "body_safety",
    "status",
    "body",
    "headers",
    "location",
    "index_files",
    "spa_fallback",
    "recorders",
    "start",
    "root_directory",
    "suffix_template",
    "append_unix_seconds",
    "rotation_interval_ms",
    "max_queue_messages",
    "max_queue_bytes",
    "shutdown_timeout_ms",
    "max_storage_bytes",
    "max_storage_files",
    "max_active_recorders",
    "methods",
    "upstream_pool",
    "connect_timeout_ms",
    "idle_timeout_ms",
    "lifetime_timeout_ms",
];

#[test]
fn exhaustively_round_trips_every_current_type_field_and_variant() {
    let (normalized, source) = normalized_round_trip(complete_config());

    assert!(source.contains("max_connections = null,"));
    assert!(source.contains("max_request_body_bytes = null,"));

    for field in RENDERED_FIELDS {
        assert!(
            source.contains(&format!("{field} =")),
            "renderer omitted `{field}`"
        );
    }

    assert_eq!(render_lua(&normalized).expect("second render"), source);
    assert_eq!(
        render_lua(&load_lua(&source).expect("reload")).expect("render after reload"),
        source
    );
}

#[test]
fn serializes_the_complete_canonical_model_for_typed_ui_data() {
    let (config, _) = normalized_round_trip(complete_config());
    let value = serde_json::to_value(config).expect("Config JSON serialization");

    assert_eq!(value["certificates"][0]["source"]["type"], "files");
    assert_eq!(value["certificates"][1]["source"]["type"], "certbot");
    assert_eq!(value["tls_profiles"][0]["min_version"], "1.3");
    assert_eq!(value["tls_profiles"][0]["alpn"][0], "h2");
    assert_eq!(value["listeners"][2]["protocol"], "tcp");
    assert_eq!(value["listeners"][0]["bind"]["type"], "socket");
    assert_eq!(value["listeners"][3]["bind"]["type"], "unix");
    assert_eq!(
        value["listeners"][3]["max_connections"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["upstream_pools"][0]["servers"][0]["endpoint"]["type"],
        "socket"
    );
    assert_eq!(
        value["upstream_pools"][0]["servers"][1]["endpoint"]["type"],
        "dns"
    );
    assert_eq!(
        value["upstream_pools"][3]["servers"][0]["endpoint"]["type"],
        "unix"
    );
    assert_eq!(value["upstream_pools"][1]["algorithm"], "least_connections");
    assert_eq!(value["upstream_pools"][0]["http_versions"]["max"], "2");
    assert_eq!(value["upstream_pools"][1]["health_check"]["type"], "http");
    assert_eq!(value["upstream_pools"][2]["health_check"]["type"], "tcp");
    assert_eq!(
        value["http_services"][0]["routes"][0]["host"]["kind"],
        "normalized_host"
    );
    assert_eq!(
        value["http_services"][0]["routes"][0]["action"]["type"],
        "proxy"
    );
    assert_eq!(
        value["http_services"][0]["routes"][1]["action"]["type"],
        "fixed_response"
    );
    assert_eq!(
        value["http_services"][0]["routes"][2]["action"]["type"],
        "redirect"
    );
    assert_eq!(
        value["http_services"][0]["routes"][3]["action"]["type"],
        "static_files"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][0]["start"],
        "continuous"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][0]["root_directory"],
        "/var/lib/oxiroute/recordings"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][1]["start"],
        "manual"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][1]["rotation_interval_ms"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["http_services"][1]["max_request_body_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["l4_services"][1]["lifetime_timeout_ms"],
        serde_json::Value::Null
    );
}

#[test]
fn emits_defaults_empty_vectors_and_options_explicitly_as_data_only_lua() {
    let source = render_lua(&minimal_config()).expect("minimal render");

    assert_eq!(
        source,
        r"return {
  version = 1,
  max_connections = null,
  management = nil,
  stats = nil,
  certificates = {
  },
  tls_profiles = {
  },
  listeners = {
  },
  cache_stores = {
  },
  upstream_pools = {
  },
  http_services = {
  },
  forward_proxy_services = {
  },
  rtmp_services = {
  },
  l4_services = {
  },
}
"
    );
    for unsupported in [
        "function", "local ", "require", "--", "[[", "]]", "(", ")", ";",
    ] {
        assert!(!source.contains(unsupported), "found `{unsupported}`");
    }
    assert_eq!(
        load_lua(&source).expect("strict loader acceptance"),
        minimal_config()
    );
}

fn identity_mutations() -> Vec<Mutation> {
    vec![
        ("management", |config| config.management = None),
        ("management.bind", |config| {
            config.management.as_mut().expect("management").bind = socket("127.0.0.1:9081");
        }),
        ("management.ui_dir", |config| {
            config.management.as_mut().expect("management").ui_dir = None;
        }),
        ("certificate order", |config| config.certificates.reverse()),
        ("certificate.dns_names", |config| {
            config.certificates[0].dns_names.reverse();
        }),
        ("certificate source paths", |config| {
            let CertificateSource::Files {
                certificate_chain_path,
                private_key_path,
            } = &mut config.certificates[0].source
            else {
                panic!("files source");
            };
            *certificate_chain_path = "/etc/oxiroute/changed-chain.pem".into();
            *private_key_path = "/etc/oxiroute/changed-key.pem".into();
        }),
        ("Certbot source paths", |config| {
            let CertificateSource::Certbot {
                live_directory_path,
                archive_directory_path,
            } = &mut config.certificates[1].source
            else {
                panic!("Certbot source");
            };
            *live_directory_path = "/etc/letsencrypt/live/changed.example.test".into();
            *archive_directory_path = "/etc/letsencrypt/archive/changed.example.test".into();
        }),
        ("TLS profile order", |config| config.tls_profiles.reverse()),
        ("TLS profile certificates", |config| {
            config.tls_profiles[0].certificates.reverse();
        }),
        ("TLS default identity", |config| {
            config.tls_profiles[0].default_certificate = "files-certificate".into();
        }),
        ("TLS minimum version", |config| {
            config.tls_profiles[0].min_version = TlsVersion::Tls12;
        }),
        ("TLS ALPN", |config| {
            config.tls_profiles[0].alpn = vec![AlpnProtocol::Http11];
        }),
        ("listener order", |config| config.listeners.reverse()),
        ("listener.bind", |config| {
            config.listeners[0].bind = ListenerBind::Socket {
                address: socket("127.0.0.1:9443"),
            };
        }),
        ("Unix listener path", |config| {
            config.listeners[3].bind = ListenerBind::Unix {
                path: "/run/oxiroute/changed-live.sock".into(),
                mode: Some(0o600),
            };
        }),
        ("listener.tls_profile", |config| {
            config.listeners[1].tls_profile = Some("files-tls".into());
        }),
        ("listener.max_connections", |config| {
            config.listeners[0].max_connections = Some(42);
        }),
    ]
}

fn upstream_mutations() -> Vec<Mutation> {
    vec![
        ("upstream pool order", |config| {
            config.upstream_pools.reverse();
        }),
        ("upstream endpoints", |config| {
            config.upstream_pools[0].servers.reverse();
        }),
        ("socket endpoint address", |config| {
            config.upstream_pools[0].servers[0].endpoint = UpstreamEndpoint::Socket {
                address: socket("10.0.0.21:443"),
            };
        }),
        ("DNS endpoint host and port", |config| {
            config.upstream_pools[0].servers[1].endpoint = UpstreamEndpoint::Dns {
                host: "CHANGED.EXAMPLE.TEST".into(),
                port: 8443,
            };
        }),
        ("Unix endpoint path", |config| {
            config.upstream_pools[3].servers[0].endpoint = UpstreamEndpoint::Unix {
                path: "/run/oxiroute/changed-backend.sock".into(),
            };
        }),
        ("upstream algorithm", |config| {
            config.upstream_pools[0].algorithm = UpstreamAlgorithm::LeastConnections;
        }),
        ("health_check", |config| {
            config.upstream_pools[1].health_check = None;
        }),
        ("health timing and thresholds", |config| {
            let health = config.upstream_pools[1]
                .health_check
                .as_mut()
                .expect("health check");
            health.interval_ms = 6_000;
            health.timeout_ms = 800;
            health.healthy_threshold = 3;
            health.unhealthy_threshold = 5;
        }),
        ("health host and path", |config| {
            let health = config.upstream_pools[1]
                .health_check
                .as_mut()
                .expect("health check");
            health.host = Some("changed.internal:3000".into());
            health.path = Some("/ready".into());
        }),
        ("upstream TLS server name", |config| {
            config.upstream_pools[0]
                .tls
                .as_mut()
                .expect("upstream TLS")
                .server_name = "CHANGED.EXAMPLE.TEST".into();
        }),
        ("upstream CA path", |config| {
            config.upstream_pools[0]
                .tls
                .as_mut()
                .expect("upstream TLS")
                .ca_certificate_path = None;
        }),
        ("HTTP version range", |config| {
            config.upstream_pools[0].http_versions.min = HttpVersion::Http2;
        }),
    ]
}

fn service_mutations() -> Vec<Mutation> {
    vec![
        ("HTTP service order", |config| {
            config.http_services.reverse();
        }),
        ("HTTP route order", |config| {
            config.http_services[0].routes.reverse();
        }),
        ("HTTP service limits", |config| {
            config.http_services[0].upstream_io_timeout_ms = 12_345;
            config.http_services[0].max_request_body_bytes = Some(54_321);
        }),
        ("route host and path", |config| {
            config.http_services[0].routes[0].host = Some(HttpHostSelector::NormalizedHost {
                value: "CHANGED.EXAMPLE.TEST".into(),
            });
            config.http_services[0].routes[0].path = HttpPathSelector::Exact {
                value: "/changed/".into(),
            };
        }),
        ("route methods", |config| {
            config.http_services[0].routes[0].methods.reverse();
        }),
        ("route upstream", |config| {
            let HttpRouteAction::Proxy {
                upstream_pool,
                policy,
            } = &mut config.http_services[0].routes[0].action
            else {
                panic!("proxy action");
            };
            *upstream_pool = "web-backends".into();
            policy.retry.max_retries = 1;
        }),
        ("L4 service order", |config| config.l4_services.reverse()),
        ("L4 service fields", |config| {
            config.l4_services[0].connect_timeout_ms = 3_000;
            config.l4_services[0].idle_timeout_ms = 4_000;
            config.l4_services[0].lifetime_timeout_ms = None;
        }),
    ]
}

fn assert_mutations_round_trip(mutations: &[Mutation]) {
    for (field, mutate) in mutations {
        let mut expected = complete_config();
        mutate(&mut expected);
        validate_config(&mut expected).unwrap_or_else(|error| panic!("{field}: {error}"));
        let source = render_lua(&expected).unwrap_or_else(|error| panic!("{field}: {error}"));
        let loaded = load_lua(&source).unwrap_or_else(|error| panic!("{field}: {error}"));
        assert_eq!(loaded, expected, "mutation lost for {field}");
    }
}

#[test]
fn preserves_individual_field_mutations() {
    assert_mutations_round_trip(&identity_mutations());
    assert_mutations_round_trip(&upstream_mutations());
    assert_mutations_round_trip(&service_mutations());
}

#[test]
fn escapes_strings_and_paths_without_changing_values() {
    let mut config = complete_config();
    config.listeners[0].name = "edge \"quoted\" \\ slash café".into();
    config.management.as_mut().expect("management").ui_dir =
        Some(PathBuf::from("/tmp/line\n\"quote\"\\tail"));

    let (normalized, source) = normalized_round_trip(config);

    assert!(source.contains(r#"name = "edge \"quoted\" \\ slash café","#));
    assert!(source.contains(r#"ui_dir = "/tmp/line\n\"quote\"\\tail","#));
    assert_eq!(load_lua(&source).expect("escaped reload"), normalized);
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_paths_without_lossy_exposure() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let mut config = minimal_config();
    config.management = Some(Management {
        bind: socket("127.0.0.1:9080"),
        ui_dir: Some(PathBuf::from(OsString::from_vec(
            b"/tmp/secret-\xff".to_vec(),
        ))),
    });

    assert!(matches!(
        render_lua(&config),
        Err(ConfigError::InvalidFilePath {
            kind: "management",
            name,
            field: "ui_dir",
            detail: "path must be valid UTF-8",
        }) if name == "management"
    ));
    assert!(serde_json::to_string(&config).is_err());

    let non_utf8 = PathBuf::from(OsString::from_vec(b"/run/oxiroute/socket-\xff".to_vec()));
    let mut listener = complete_config();
    listener.listeners[3].bind = ListenerBind::Unix {
        path: non_utf8.clone(),
        mode: None,
    };
    assert!(matches!(
        render_lua(&listener),
        Err(ConfigError::InvalidUnixPath {
            kind: "listener",
            name,
            field: "bind.path",
            detail: "path must be valid UTF-8",
        }) if name == "live"
    ));

    let mut endpoint = complete_config();
    endpoint.upstream_pools[3].servers[0].endpoint = UpstreamEndpoint::Unix { path: non_utf8 };
    assert!(matches!(
        render_lua(&endpoint),
        Err(ConfigError::InvalidUnixPath {
            kind: "upstream pool",
            name,
            field: "endpoints[].path",
            detail: "path must be valid UTF-8",
        }) if name == "unix-backends"
    ));

    let mut recorder = complete_config();
    recorder.rtmp_services[0].applications[0].recorders[0].root_directory = PathBuf::from(
        OsString::from_vec(b"/var/lib/oxiroute/recordings-\xff".to_vec()),
    );
    assert!(matches!(
        render_lua(&recorder),
        Err(ConfigError::InvalidRtmpRecorderPolicy {
            service,
            application,
            recorder,
            field: "root_directory",
            detail: "path must be valid UTF-8",
        }) if service == "live" && application == "live" && recorder == "continuous"
    ));

    let mut access = complete_config();
    let Some(HttpAccessPolicy::BearerTokenFile {
        token_file_path, ..
    }) = &mut access.http_services[0].routes[0].access_policy
    else {
        panic!("bearer access policy");
    };
    *token_file_path = PathBuf::from(OsString::from_vec(b"/run/secrets/token-\xff".to_vec()));
    assert!(matches!(
        render_lua(&access),
        Err(ConfigError::InvalidFilePath {
            kind: "HTTP access policy",
            field: "token_file_path",
            detail: "path must be valid UTF-8",
            ..
        })
    ));

    let mut static_files = complete_config();
    let HttpRouteAction::StaticFiles { root_directory, .. } =
        &mut static_files.http_services[0].routes[3].action
    else {
        panic!("static-files action");
    };
    *root_directory = PathBuf::from(OsString::from_vec(b"/srv/www-\xff".to_vec()));
    assert!(matches!(
        render_lua(&static_files),
        Err(ConfigError::InvalidHttpRoute {
            service,
            route: 3,
            field: "action.static_files.root_directory",
            detail,
        }) if service == "web" && detail == "path must be valid UTF-8"
    ));
}

#[test]
fn rejects_u64_values_that_json_and_lua_cannot_round_trip_exactly() {
    let mutations: &[Mutation] = &[
        ("upstream_io_timeout_ms", |config| {
            config.http_services[0].upstream_io_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("max_request_body_bytes", |config| {
            config.http_services[0].max_request_body_bytes = Some(MAX_SAFE_INTEGER + 1);
        }),
        ("connect_timeout_ms", |config| {
            config.l4_services[0].connect_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("idle_timeout_ms", |config| {
            config.l4_services[0].idle_timeout_ms = MAX_SAFE_INTEGER + 1;
        }),
        ("lifetime_timeout_ms", |config| {
            config.l4_services[0].lifetime_timeout_ms = Some(MAX_SAFE_INTEGER + 1);
        }),
    ];

    for (expected_field, mutate) in mutations {
        let mut config = complete_config();
        mutate(&mut config);
        assert!(matches!(
            render_lua(&config),
            Err(ConfigError::LimitTooLarge { field, .. }) if field == *expected_field
        ));
    }
}

#[test]
fn rejects_rendered_sources_beyond_the_loader_limit() {
    let mut config = minimal_config();
    config.listeners.push(Listener {
        name: "x".repeat(1024 * 1024),
        bind: ListenerBind::Socket {
            address: socket("127.0.0.1:1935"),
        },
        protocol: Protocol::Rtmp,
        service: Some("live".into()),
        tls_profile: None,
        max_connections: Some(1),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    config.rtmp_services = test_rtmp_services();

    assert!(matches!(
        render_lua(&config),
        Err(ConfigError::SourceTooLarge)
    ));
}
