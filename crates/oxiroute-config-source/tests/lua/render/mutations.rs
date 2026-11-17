use crate::fixtures::{complete_config, socket};
use crate::lua_support::{load_lua, render_lua};
use oxiroute_config::{
    AccessLogPolicy, AlpnProtocol, CertificateSource, ConfigDraft, HttpHostSelector,
    HttpPathSelector, HttpRouteAction, HttpVersion, ListenerBind, TlsVersion, UpstreamAlgorithm,
    UpstreamEndpoint,
};

pub(crate) type Mutation = (&'static str, fn(&mut ConfigDraft));

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
                ipv6_only: false,
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
                address: socket("192.0.2.29:443"),
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
                .server_name = Some("CHANGED.EXAMPLE.TEST".into());
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
            policy.rescue = None;
            policy.retry.max_retries = 1;
        }),
        ("L4 service order", |config| config.l4_services.reverse()),
        ("L4 service fields", |config| {
            config.l4_services[0].connect_timeout_ms = 3_000;
            config.l4_services[0].idle_timeout_ms = 4_000;
            config.l4_services[0].lifetime_timeout_ms = None;
        }),
        ("RTMP message and acknowledgement limits", |config| {
            config.rtmp_services[0].max_inbound_message_size = 2 * 1024 * 1024;
            config.rtmp_services[0].ack_window_size = 1_000_000;
        }),
        ("RTMP access log file policy", |config| {
            config.rtmp_services[0].access_log = Some(AccessLogPolicy::File {
                path: "/var/log/oxiroute/rtmp-access.jsonl".into(),
            });
        }),
        ("RTMP access log disabled policy", |config| {
            config.rtmp_services[0].access_log = Some(AccessLogPolicy::Disabled);
        }),
    ]
}

fn assert_mutations_round_trip(mutations: &[Mutation]) {
    for (field, mutate) in mutations {
        let mut expected = complete_config();
        mutate(&mut expected);
        let expected = expected
            .validate()
            .unwrap_or_else(|error| panic!("{field}: {error}"))
            .to_draft();
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
