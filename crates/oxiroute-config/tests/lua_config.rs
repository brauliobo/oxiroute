use oxiroute_config::{
    AcmeChallengeType, AcmeKeyType, AlpnProtocol, CertificateSource, ConfigError, HealthCheckType,
    HttpVersion, ListenerBind, Protocol, SelfSignedKeyType, StatsPageAdminPolicy, TlsVersion,
    UpstreamAlgorithm, UpstreamEndpoint, load_lua, render_lua,
};

#[test]
fn loads_multiple_ipv4_ipv6_statistics_binds_and_admin_token_path() {
    let config = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            binds = { "127.0.0.1:8404", "[::1]:8404" },
            admin_token_file = "/etc/oxiroute/stats.token",
          },
        }"#,
    )
    .expect("statistics config");
    let stats = config.stats.expect("statistics");
    assert_eq!(stats.binds.len(), 2);
    assert_eq!(
        stats.admin_token_file.as_deref(),
        Some(std::path::Path::new("/etc/oxiroute/stats.token"))
    );
}

#[test]
fn rejects_statistics_binds_that_overlap_management() {
    let error = load_lua(
        r#"return {
          version = 1,
          management = { bind = "127.0.0.1:8404" },
          stats = { binds = { "0.0.0.0:8404" } },
          listeners = {},
        }"#,
    )
    .expect_err("overlap");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}

#[test]
fn page_only_statistics_validate_render_and_roundtrip() {
    let config = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            pages = {
              {
                bind = "127.0.0.1:8404",
                uri_prefix = "/stats",
                refresh_ms = 10000,
                admin = "localhost",
                max_connections = 250,
                downstream_timeouts = {
                  client_timeout_ms = 600000,
                  request_timeout_ms = 600000,
                  keepalive_timeout_ms = 60000,
                },
              },
            },
          },
        }"#,
    )
    .expect("page-only statistics config");
    let page = &config.stats.as_ref().expect("statistics").pages[0];
    assert!(config.stats.as_ref().expect("statistics").binds.is_empty());
    assert_eq!(page.uri_prefix, "/stats");
    assert_eq!(page.refresh_ms, 10_000);
    assert_eq!(page.admin, StatsPageAdminPolicy::Localhost);
    assert_eq!(page.max_connections, Some(250));
    assert_eq!(page.downstream_timeouts.client_timeout_ms, Some(600_000));
    assert_eq!(page.downstream_timeouts.request_timeout_ms, Some(600_000));
    assert_eq!(page.downstream_timeouts.keepalive_timeout_ms, Some(60_000));

    let rendered = render_lua(&config).expect("render page");
    let roundtrip = load_lua(&rendered).expect("roundtrip page");
    assert_eq!(roundtrip, config);
}

#[test]
fn statistics_page_uri_prefix_requires_an_exact_ascii_http_path() {
    for uri in ["/café1", "/bad|path", "/bad^path"] {
        let source = format!(
            r#"return {{
              version = 1,
              listeners = {{}},
              stats = {{ pages = {{{{
                bind = "127.0.0.1:8404",
                uri_prefix = "{uri}",
                refresh_ms = 1000,
                admin = "disabled",
              }}}} }},
            }}"#
        );
        assert!(matches!(
            load_lua(&source).expect_err("invalid statistics page URI"),
            ConfigError::InvalidStatsPage {
                field: "uri_prefix",
                ..
            }
        ));
    }
}

#[test]
fn rejects_invalid_statistics_pages_and_broad_bind_conflicts() {
    for (field, value) in [
        ("uri_prefix", "uri_prefix = \"relative\", refresh_ms = 1000"),
        ("refresh_ms", "uri_prefix = \"/stats\", refresh_ms = 0"),
        (
            "refresh_ms",
            "uri_prefix = \"/stats\", refresh_ms = 86400001",
        ),
    ] {
        let source = format!(
            r#"return {{
              version = 1,
              listeners = {{}},
              stats = {{
                pages = {{{{
                  bind = "127.0.0.1:8404",
                  {value},
                  admin = "disabled",
                }}}},
              }},
            }}"#
        );
        let error = load_lua(&source).expect_err("invalid page");
        assert!(
            matches!(error, ConfigError::InvalidStatsPage { field: actual, .. } if actual == field)
        );
    }

    let zero_limit = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = { pages = {{
            bind = "127.0.0.1:8404",
            uri_prefix = "/stats",
            refresh_ms = 1000,
            admin = "disabled",
            max_connections = 0,
          }} },
        }"#,
    )
    .expect_err("zero statistics page limit");
    assert!(matches!(zero_limit, ConfigError::ZeroLimit { .. }));

    let zero_timeout = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = { pages = {{
            bind = "127.0.0.1:8404",
            uri_prefix = "/stats",
            refresh_ms = 1000,
            admin = "disabled",
            downstream_timeouts = { request_timeout_ms = 0 },
          }} },
        }"#,
    )
    .expect_err("zero statistics page timeout");
    assert!(matches!(
        zero_timeout,
        ConfigError::InvalidStatsPage {
            field: "downstream_timeouts.request_timeout_ms",
            ..
        }
    ));

    let error = load_lua(
        r#"return {
          version = 1,
          listeners = {},
          stats = {
            binds = { "127.0.0.1:8404" },
            pages = {{
              bind = "0.0.0.0:8404",
              uri_prefix = "/stats",
              refresh_ms = 1000,
              admin = "disabled",
            }},
          },
        }"#,
    )
    .expect_err("broad page bind conflict");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}

#[test]
fn statistics_total_bind_count_is_bounded_across_legacy_and_page_binds() {
    let pages = (1..=8)
        .map(|port| {
            format!(
                "{{ bind = \"127.0.0.1:{}\", uri_prefix = \"/stats\", refresh_ms = 1000, admin = \"disabled\" }}",
                8404 + port
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let source = format!(
        r#"return {{
          version = 1,
          listeners = {{}},
          stats = {{ binds = {{ "127.0.0.1:8404" }}, pages = {{ {pages} }} }},
        }}"#
    );

    assert!(matches!(
        load_lua(&source).expect_err("nine total binds"),
        ConfigError::InvalidStatsBinds
    ));
}

#[test]
fn statistics_page_uri_prefix_is_bounded() {
    let uri = format!("/{}", "a".repeat(2_048));
    let source = format!(
        r#"return {{
          version = 1,
          listeners = {{}},
          stats = {{ pages = {{{{
            bind = "127.0.0.1:8404",
            uri_prefix = "{uri}",
            refresh_ms = 1000,
            admin = "disabled",
          }}}} }},
        }}"#
    );

    assert!(matches!(
        load_lua(&source).expect_err("oversized page URI"),
        ConfigError::InvalidStatsPage {
            field: "uri_prefix",
            ..
        }
    ));
}

#[test]
fn statistics_pages_conflict_with_management_and_other_broad_pages() {
    for stats in [
        r#"management = { bind = "127.0.0.1:8404" },
           stats = { pages = {{
             bind = "0.0.0.0:8404", uri_prefix = "/stats", refresh_ms = 1000, admin = "disabled",
           }} },"#,
        r#"stats = { pages = {
             { bind = "127.0.0.1:8404", uri_prefix = "/one", refresh_ms = 1000, admin = "disabled" },
             { bind = "0.0.0.0:8404", uri_prefix = "/two", refresh_ms = 1000, admin = "disabled" },
           } },"#,
    ] {
        let source = format!(
            r"return {{
              version = 1,
              listeners = {{}},
              {stats}
            }}"
        );
        assert!(matches!(
            load_lua(&source).expect_err("page bind overlap"),
            ConfigError::OverlappingBind { .. }
        ));
    }

    let error = load_lua(
        r#"return {
          version = 1,
          stats = { pages = {{
            bind = "0.0.0.0:8404", uri_prefix = "/stats", refresh_ms = 1000, admin = "disabled",
          }} },
          listeners = {{
            name = "web",
            bind = { type = "socket", address = "127.0.0.1:8404" },
            protocol = "http",
            service = "web",
          }},
          http_services = {{
            name = "web",
            routes = {{
              path = { kind = "exact", value = "/" },
              action = { type = "fixed_response", status = 200 },
            }},
          }},
        }"#,
    )
    .expect_err("page/listener overlap");
    assert!(matches!(error, ConfigError::OverlappingBind { .. }));
}

const VALID_CONFIG: &str = r#"
return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  certificates = {
    {
      name = "web-certificate",
      dns_names = { "WWW.EXAMPLE.TEST", "*.EXAMPLE.TEST" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },
    },
  },
  tls_profiles = {
    {
      name = "web-tls",
      certificates = { "web-certificate" },
      default_certificate = "web-certificate",
      min_version = "1.3",
      alpn = { "h2", "http/1.1" },
    },
  },
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
      tls_profile = "web-tls",
      max_connections = 5000,
    },
    {
      name = "database",
      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp",
      service = "database",
      max_connections = 1000,
    },
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
      max_connections = 500,
    },
  },
  upstream_pools = {
    {
      name = "web-backends",
      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin",
    },
    {
      name = "database-backends",
      endpoints = { { type = "socket", address = "10.0.0.12:5432" } },
      algorithm = "round_robin",
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "GET", "POST" },
          action = {
            type = "proxy",
            upstream_pool = "web-backends",
            policy = {},
          },
        },
      },
      upstream_io_timeout_ms = 15000,
      max_request_body_bytes = 2097152,
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        { name = "live", live = true, idle_streams = true },
      },
    },
  },
  l4_services = {
    {
      name = "database",
      upstream_pool = "database-backends",
      connect_timeout_ms = 5000,
      idle_timeout_ms = 120000,
      lifetime_timeout_ms = 600000,
    },
  },
}
"#;

const WEB_ROUTES: &str = r#"      routes = {
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "GET", "POST" },
          action = {
            type = "proxy",
            upstream_pool = "web-backends",
            policy = {},
          },
        },
      },"#;

fn changed(from: &str, to: &str) -> String {
    assert_eq!(
        VALID_CONFIG.matches(from).count(),
        1,
        "fixture fragment must occur exactly once: {from}"
    );
    VALID_CONFIG.replacen(from, to, 1)
}

fn error_from(source: &str) -> ConfigError {
    load_lua(source).expect_err("configuration must be rejected")
}

fn with_web_pool_fields(fields: &str) -> String {
    let pool = r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin","#;
    changed(pool, &format!("{pool}\n{fields}"))
}

fn upstream_tls(server_name: &str) -> String {
    format!(
        r#"      tls = {{ server_name = "{server_name}", ca_certificate_path = "/etc/oxiroute/upstream-ca.pem" }},"#
    )
}

fn with_certbot_source(live_directory_path: &str, archive_directory_path: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "certbot",
        live_directory_path = "{live_directory_path}",
        archive_directory_path = "{archive_directory_path}",
      }},"#
        ),
    )
}

fn with_self_signed_source(fields: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "self_signed_development",
        {fields}
      }},"#
        ),
    )
}

fn with_acme_source(fields: &str) -> String {
    changed(
        r#"      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },"#,
        &format!(
            r#"      source = {{
        type = "acme_managed",
        directory_url = "https://acme.example.test/directory",
        state_root = "/var/lib/oxiroute/acme",
        terms_agreed = true,
        allowed_dns_suffixes = {{ "example.test" }},
        {fields}
      }},"#
        ),
    )
    .replace(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"WWW.EXAMPLE.TEST\" },",
    )
}

#[test]
fn loads_the_canonical_configuration() {
    let config = load_lua(VALID_CONFIG).expect("valid canonical configuration");

    assert_eq!(config.version, 1);
    assert_eq!(
        config.management.as_ref().expect("management").bind.port(),
        9080
    );
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(
        config.certificates[0].dns_names,
        ["www.example.test", "*.example.test"]
    );
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::Files { .. }
    ));
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.tls_profiles[0].certificates, ["web-certificate"]);
    assert_eq!(
        config.tls_profiles[0].default_certificate,
        "web-certificate"
    );
    assert_eq!(config.tls_profiles[0].min_version, TlsVersion::Tls13);
    assert_eq!(
        config.tls_profiles[0].alpn,
        [AlpnProtocol::H2, AlpnProtocol::Http11]
    );
    assert_eq!(config.listeners.len(), 3);
    assert!(config.cache_stores.is_empty());
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("socket address")
        }
    );
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert_eq!(config.listeners[0].service.as_deref(), Some("web"));
    assert_eq!(config.listeners[0].tls_profile.as_deref(), Some("web-tls"));
    assert_eq!(config.listeners[1].protocol, Protocol::Tcp);
    assert_eq!(config.listeners[2].protocol, Protocol::Rtmp);
    assert_eq!(config.listeners[2].service.as_deref(), Some("live"));
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(
        config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Socket {
            address: "127.0.0.1:3000".parse().expect("socket address")
        }
    );
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(config.upstream_pools[0].tls, None);
    assert_eq!(
        config.upstream_pools[0].http_versions.min,
        HttpVersion::Http11
    );
    assert_eq!(
        config.upstream_pools[0].http_versions.max,
        HttpVersion::Http11
    );
    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["host"]
            ["value"],
        "example.com"
    );
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, Some(600_000));
    assert_eq!(config.rtmp_services[0].applications[0].name, "live");
    assert!(config.rtmp_services[0].applications[0].live);
    assert!(config.rtmp_services[0].applications[0].idle_streams);
    assert!(config.rtmp_services[0].applications[0].recorders.is_empty());
}

#[test]
fn loads_a_bounded_udp_service_and_listener() {
    let source = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp","#,
        r#"      bind = { type = "udp", address = "127.0.0.1:15432" },
      protocol = "udp","#,
    )
    .replace(
        "      lifetime_timeout_ms = 600000,\n",
        "      lifetime_timeout_ms = 600000,\n      udp = { max_datagram_bytes = 1200, max_sessions = 16, max_session_bytes = 65536, max_queue_datagrams = 4, max_queue_bytes = 8192 },\n",
    );
    let config = load_lua(&source).expect("bounded UDP configuration");
    assert_eq!(config.listeners[1].protocol, Protocol::Udp);
    assert!(matches!(
        config.listeners[1].bind,
        ListenerBind::Udp { address } if address == "127.0.0.1:15432".parse().expect("UDP bind")
    ));
    let policy = config.l4_services[0].udp.expect("UDP policy");
    assert_eq!(policy.max_datagram_bytes, 1200);
    assert_eq!(policy.max_sessions, 16);
    assert_eq!(policy.max_session_bytes, 65_536);
    assert_eq!(policy.max_queue_datagrams, 4);
    assert_eq!(policy.max_queue_bytes, 8192);
}

#[test]
fn rejects_an_unbounded_udp_policy() {
    let source = changed(
        "      lifetime_timeout_ms = 600000,",
        "      lifetime_timeout_ms = 600000,\n      udp = { max_datagram_bytes = 0 },",
    );
    assert!(matches!(
        load_lua(&source),
        Err(ConfigError::InvalidL4UdpPolicy { field, .. })
            if field == "udp.max_datagram_bytes"
    ));
}

#[test]
fn self_signed_development_source_defaults_and_renders_explicitly() {
    let config = load_lua(&with_self_signed_source("")).expect("default development source");
    let CertificateSource::SelfSignedDevelopment {
        validity_days,
        key_type,
    } = config.certificates[0].source
    else {
        panic!("development source");
    };
    assert_eq!(validity_days, 7);
    assert_eq!(key_type, SelfSignedKeyType::EcdsaP256);

    let rendered = render_lua(&config).expect("render development source");
    assert!(rendered.contains("type = \"self_signed_development\""));
    assert!(rendered.contains("validity_days = 7"));
    assert!(rendered.contains("key_type = \"ecdsa_p256\""));
    assert_eq!(
        load_lua(&rendered).expect("reload development source"),
        config
    );
}

#[test]
fn self_signed_development_source_accepts_key_type_and_rejects_unbounded_validity() {
    let config = load_lua(&with_self_signed_source(
        "validity_days = 14,\n        key_type = \"rsa_2048\",",
    ))
    .expect("bounded RSA development source");
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::SelfSignedDevelopment {
            validity_days: 14,
            key_type: SelfSignedKeyType::Rsa2048,
        }
    ));

    for validity_days in [0, 31] {
        let error = load_lua(&with_self_signed_source(&format!(
            "validity_days = {validity_days},"
        )))
        .expect_err("out-of-bounds development validity");
        assert!(matches!(
            error,
            ConfigError::InvalidSelfSignedValidityDays { value, .. } if value == validity_days
        ));
    }
}

#[test]
fn managed_acme_source_round_trips_and_rejects_unsafe_policy_values() {
    let config = load_lua(&with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"http01\",\n        key_type = \"rsa_2048\",",
    ))
    .expect("managed ACME source");
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::Http01,
            key_type: AcmeKeyType::Rsa2048,
            dns01: None,
            ..
        }
    ));
    let rendered = render_lua(&config).expect("render managed ACME source");
    assert!(rendered.contains("type = \"acme_managed\""));
    assert_eq!(
        load_lua(&rendered).expect("reload managed ACME source"),
        config
    );

    for (field, value) in [
        ("directory_url", "directory_url = \"https://\""),
        (
            "directory_url",
            "directory_url = \"http://acme.example.test/directory\"",
        ),
        (
            "allowed_dns_suffixes",
            "allowed_dns_suffixes = { \"other.test\" }",
        ),
        ("retained_revisions", "retained_revisions = 0"),
    ] {
        let error = load_lua(&with_acme_source(value)).expect_err("invalid managed ACME source");
        assert!(
            matches!(
                (field, &error),
                ("directory_url", ConfigError::InvalidAcmeDirectoryUrl { .. })
                    | (
                        "allowed_dns_suffixes",
                        ConfigError::AcmeIdentifierOutsidePolicy { .. }
                    )
                    | (
                        "retained_revisions",
                        ConfigError::InvalidAcmeRetention { .. }
                    )
            ),
            "unexpected error for {field}: {error:?}"
        );
    }

    let tls_alpn_config = load_lua(&with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"tls_alpn01\",\n        key_type = \"ecdsa_p256\",",
    ))
    .expect("managed TLS-ALPN-01 source");
    assert!(matches!(
        tls_alpn_config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::TlsAlpn01,
            dns01: None,
            ..
        }
    ));
    assert_eq!(
        load_lua(&render_lua(&tls_alpn_config).expect("render TLS-ALPN-01 source"))
            .expect("reload TLS-ALPN-01 source"),
        tls_alpn_config
    );

    let dns_config = with_acme_source(
        "contacts = { \"mailto:ops@example.test\" },\n        challenge = \"dns01\",\n        key_type = \"ecdsa_p256\",\n        dns01 = { provider = \"fake\", credential_file = \"/etc/oxiroute/dns-credentials\", timeout_seconds = 30 },",
    )
    .replace(
        "      dns_names = { \"WWW.EXAMPLE.TEST\" },",
        "      dns_names = { \"*.EXAMPLE.TEST\" },",
    );
    let dns_config = load_lua(&dns_config).expect("managed DNS-01 source");
    assert!(matches!(
        &dns_config.certificates[0].source,
        CertificateSource::AcmeManaged {
            challenge: AcmeChallengeType::Dns01,
            dns01: Some(dns01),
            ..
        } if dns01.provider == "fake"
            && dns01.credential_file
                == std::path::PathBuf::from("/etc/oxiroute/dns-credentials")
            && dns01.timeout_seconds == 30
    ));
    assert_eq!(
        load_lua(&render_lua(&dns_config).expect("render DNS-01 source"))
            .expect("reload DNS-01 source"),
        dns_config
    );
}

#[test]
fn loads_the_distributed_example_configuration() {
    let config = load_lua(include_str!("../../../oxiroute.example.lua"))
        .expect("distributed example must remain valid");

    assert_eq!(config.listeners.len(), 3);
    assert!(config.cache_stores.is_empty());
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(config.http_services.len(), 1);
    assert!(config.forward_proxy_services.is_empty());
    assert_eq!(config.rtmp_services.len(), 1);
    assert!(config.rtmp_services[0].applications[0].recorders.is_empty());
    assert_eq!(config.l4_services.len(), 1);
}

#[test]
#[allow(clippy::too_many_lines)]
fn applies_all_collection_and_field_defaults() {
    let minimal = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        { name = "live", live = true },
      },
    },
  },
}
"#,
    )
    .expect("minimal configuration");

    assert_eq!(minimal.management, None);
    assert_eq!(minimal.max_connections, None);
    assert!(minimal.certificates.is_empty());
    assert!(minimal.tls_profiles.is_empty());
    assert_eq!(minimal.listeners[0].max_connections, None);
    assert_eq!(
        minimal.listeners[0].downstream_timeouts,
        oxiroute_config::DownstreamTimeoutPolicy::default()
    );
    assert_eq!(minimal.listeners[0].tls_profile, None);
    assert!(minimal.upstream_pools.is_empty());
    assert!(minimal.http_services.is_empty());
    assert!(minimal.rtmp_services[0].applications[0].idle_streams);
    assert_eq!(minimal.rtmp_services[0].outbound_chunk_size, 4_096);
    assert_eq!(minimal.rtmp_services[0].access_log, None);
    assert_eq!(
        minimal.rtmp_services[0].applications[0]
            .fanout
            .max_subscribers,
        1_024
    );
    assert!(
        minimal.rtmp_services[0].applications[0]
            .recorders
            .is_empty()
    );
    assert!(minimal.l4_services.is_empty());

    let source = VALID_CONFIG
        .replace("      max_connections = 5000,\n", "")
        .replace("      max_connections = 1000,\n", "")
        .replace("      max_connections = 500,\n", "")
        .replace("      algorithm = \"round_robin\",\n", "")
        .replace(
            "          host = { kind = \"normalized_host\", value = \"example.com\" },\n",
            "",
        )
        .replace("          methods = { \"GET\", \"POST\" },\n", "")
        .replace("      upstream_io_timeout_ms = 15000,\n", "")
        .replace("      max_request_body_bytes = 2097152,\n", "")
        .replace("      connect_timeout_ms = 5000,\n", "")
        .replace("      idle_timeout_ms = 120000,\n", "")
        .replace("      lifetime_timeout_ms = 600000,\n", "");
    let source = source
        .replace("      min_version = \"1.3\",\n", "")
        .replace("      alpn = { \"h2\", \"http/1.1\" },\n", "");
    let config = load_lua(&source).expect("configuration using field defaults");
    let route = &config.http_services[0].routes[0];

    assert!(
        config
            .listeners
            .iter()
            .all(|listener| listener.max_connections.is_none())
    );
    assert!(
        config
            .upstream_pools
            .iter()
            .all(|pool| pool.algorithm == UpstreamAlgorithm::RoundRobin)
    );
    assert_eq!(route.host, None);
    assert_eq!(
        serde_json::to_value(route).expect("serialized route")["path"]["value"],
        "/api"
    );
    assert!(route.methods.is_empty());
    assert_eq!(route.policy.connect_timeout_ms, 30_000);
    assert_eq!(route.policy.read_timeout_ms, 30_000);
    assert_eq!(route.policy.write_timeout_ms, 30_000);
    assert!(!route.policy.request_buffering);
    assert!(!route.policy.response_buffering);
    assert_eq!(config.http_services[0].upstream_io_timeout_ms, 30_000);
    assert_eq!(
        serde_json::to_value(route).expect("serialized route")["action"]["policy"]["retry"]["max_retries"],
        0
    );
    assert_eq!(
        config.http_services[0].max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    assert_eq!(config.l4_services[0].connect_timeout_ms, 10_000);
    assert_eq!(config.l4_services[0].idle_timeout_ms, 300_000);
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, None);
    assert_eq!(config.tls_profiles[0].min_version, TlsVersion::Tls12);
    assert_eq!(config.tls_profiles[0].alpn, [AlpnProtocol::Http11]);
    assert_eq!(
        config.tls_profiles[0].policy,
        oxiroute_config::TlsPolicy::default()
    );
    assert!(config.upstream_pools.iter().all(|pool| {
        pool.tls.is_none()
            && pool.http_versions.min == HttpVersion::Http11
            && pool.http_versions.max == HttpVersion::Http11
            && pool.connection_reuse == oxiroute_config::UpstreamConnectionReuse::Safe
    }));
}

#[test]
fn applies_optional_admission_defaults_without_fabricating_limits() {
    let listener_omitted = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  rtmp_services = {
    { name = "live", applications = { { name = "live" } } },
  },
}
"#,
    )
    .expect("omitted listener limit");
    assert_eq!(listener_omitted.listeners[0].max_connections, None);

    let listener_null = changed(
        "      max_connections = 5000,",
        "      max_connections = null,",
    );
    assert_eq!(
        load_lua(&listener_null)
            .expect("null listener limit")
            .listeners[0]
            .max_connections,
        None
    );
    assert_eq!(
        load_lua(VALID_CONFIG)
            .expect("numeric listener limit")
            .listeners[0]
            .max_connections,
        Some(5_000)
    );

    let body_omitted = changed("      max_request_body_bytes = 2097152,\n", "");
    assert_eq!(
        load_lua(&body_omitted)
            .expect("omitted body limit")
            .http_services[0]
            .max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    let body_null = changed(
        "      max_request_body_bytes = 2097152,",
        "      max_request_body_bytes = null,",
    );
    assert_eq!(
        load_lua(&body_null).expect("null body limit").http_services[0].max_request_body_bytes,
        None
    );
    let body_nil = changed(
        "      max_request_body_bytes = 2097152,",
        "      max_request_body_bytes = nil,",
    );
    assert_eq!(
        load_lua(&body_nil)
            .expect("nil is an omitted Lua field")
            .http_services[0]
            .max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    assert_eq!(
        load_lua(VALID_CONFIG)
            .expect("numeric body limit")
            .http_services[0]
            .max_request_body_bytes,
        Some(2_097_152)
    );
}

#[test]
fn applies_the_same_optional_admission_contract_to_json() {
    let base = serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "live",
            "bind": { "type": "socket", "address": "127.0.0.1:1935" },
            "protocol": "rtmp"
        }],
        "http_services": [{ "name": "web", "routes": [] }]
    });

    let omitted: oxiroute_config::Config =
        serde_json::from_value(base.clone()).expect("omitted JSON limits");
    assert_eq!(omitted.max_connections, None);
    assert_eq!(omitted.listeners[0].max_connections, None);
    assert_eq!(
        omitted.http_services[0].max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );

    let mut explicit_null = base.clone();
    explicit_null["listeners"][0]["max_connections"] = serde_json::Value::Null;
    explicit_null["max_connections"] = serde_json::Value::Null;
    explicit_null["http_services"][0]["max_request_body_bytes"] = serde_json::Value::Null;
    let explicit_null: oxiroute_config::Config =
        serde_json::from_value(explicit_null).expect("null JSON limits");
    assert_eq!(explicit_null.listeners[0].max_connections, None);
    assert_eq!(explicit_null.max_connections, None);
    assert_eq!(explicit_null.http_services[0].max_request_body_bytes, None);

    let mut numeric = base;
    numeric["listeners"][0]["max_connections"] = 321.into();
    numeric["max_connections"] = 123.into();
    numeric["http_services"][0]["max_request_body_bytes"] = 654.into();
    let numeric: oxiroute_config::Config =
        serde_json::from_value(numeric).expect("numeric JSON limits");
    assert_eq!(numeric.listeners[0].max_connections, Some(321));
    assert_eq!(numeric.max_connections, Some(123));
    assert_eq!(numeric.http_services[0].max_request_body_bytes, Some(654));
}

#[test]
fn requires_explicit_tagged_listener_and_upstream_objects() {
    let old_listener = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
        r#"      bind = "127.0.0.1:8080","#,
    );
    assert!(matches!(error_from(&old_listener), ConfigError::Lua(_)));

    let old_endpoint = changed(
        r#"        { type = "socket", address = "127.0.0.1:3000" },"#,
        r#"        "127.0.0.1:3000","#,
    );
    assert!(matches!(error_from(&old_endpoint), ConfigError::Lua(_)));

    for source in [
        changed(
            r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
            r#"      bind = { type = "socket", address = "127.0.0.1:8080", path = "/run/web.sock" },"#,
        ),
        changed(
            r#"        { type = "socket", address = "127.0.0.1:3000" },"#,
            r#"        { type = "socket", address = "127.0.0.1:3000", host = "backend.test" },"#,
        ),
    ] {
        let error = error_from(&source);
        assert!(matches!(error, ConfigError::Lua(_)));
        assert!(error.to_string().contains("unknown field"));
    }
}

#[test]
fn loads_and_normalizes_every_bind_and_endpoint_variant() {
    let source = r#"
return {
  version = 1,
  listeners = {
    {
      name = "local",
      bind = { type = "unix", path = "/run//oxiroute///local.sock" },
      protocol = "rtmp",
      service = "live",
      max_connections = null,
    },
  },
  upstream_pools = {
    {
      name = "all-endpoints",
      endpoints = {
        { type = "socket", address = "[::ffff:127.0.0.1]:3000" },
        { type = "dns", host = "BACKEND-1.EXAMPLE.TEST", port = 3001 },
        { type = "unix", path = "/run//oxiroute///backend.sock" },
      },
      algorithm = "least_connections",
    },
  },
  rtmp_services = {
    { name = "live", applications = { { name = "live" } } },
  },
}
"#;
    let config = load_lua(source).expect("all bind and endpoint variants");

    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Unix {
            path: "/run/oxiroute/local.sock".into(),
            mode: None,
        }
    );
    assert_eq!(config.listeners[0].max_connections, None);
    assert_eq!(
        config.upstream_pools[0]
            .servers
            .iter()
            .map(|server| server.endpoint.clone())
            .collect::<Vec<_>>(),
        [
            UpstreamEndpoint::Socket {
                address: "127.0.0.1:3000".parse().expect("socket address")
            },
            UpstreamEndpoint::Dns {
                host: "backend-1.example.test".into(),
                port: 3001
            },
            UpstreamEndpoint::Unix {
                path: "/run/oxiroute/backend.sock".into()
            },
        ]
    );
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::LeastConnections
    );
}

#[test]
fn validates_and_normalizes_dns_endpoints_without_resolution_or_expansion() {
    let boundary = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "dns", endpoints = {{ {{ type = "dns", host = "{boundary}", port = 443 }} }} }},
  }},
}}"#
    );
    let config = load_lua(&source).expect("253-byte DNS endpoint");
    assert!(matches!(
        &config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Dns { host, port: 443 } if host == &boundary
    ));

    let too_long = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    for host in [
        String::new(),
        "127.0.0.1".into(),
        "::1".into(),
        "example.test.".into(),
        "-api.example.test".into(),
        "api-.example.test".into(),
        "api..example.test".into(),
        "api_example.test".into(),
        "caf\u{e9}.example.test".into(),
        "${BACKEND_HOST}".into(),
        format!("{}.example.test", "a".repeat(64)),
        too_long,
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "dns", endpoints = {{ {{ type = "dns", host = "{host}", port = 443 }} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidDnsEndpoint { pool, .. } if pool == "dns"
        ));
    }

    let zero_port = r#"return { version = 1, listeners = {}, upstream_pools = {
  { name = "dns", endpoints = { { type = "dns", host = "backend.test", port = 0 } } },
} }"#;
    assert!(matches!(
        error_from(zero_port),
        ConfigError::ZeroPort { kind: "upstream pool", name, field: "endpoints" }
            if name == "dns"
    ));
}

#[test]
fn validates_normalizes_and_deduplicates_unix_paths() {
    let boundary = format!("/{}", "a".repeat(106));
    let source = format!(
        r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "unix", endpoints = {{ {{ type = "unix", path = "{boundary}" }} }} }},
}} }}"#
    );
    load_lua(&source).expect("107-byte Unix path");

    for path in [
        "run/backend.sock".to_owned(),
        "/".to_owned(),
        "/run/backend.sock/".to_owned(),
        "/run/./backend.sock".to_owned(),
        "/run/../backend.sock".to_owned(),
        r"/run/\0backend.sock".to_owned(),
        format!("/{}", "a".repeat(107)),
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "unix", endpoints = {{ {{ type = "unix", path = "{path}" }} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidUnixPath { kind: "upstream pool", name, .. }
                if name == "unix"
        ));
    }

    let duplicates = r#"return { version = 1, listeners = {}, upstream_pools = {
  { name = "unix", endpoints = {
    { type = "unix", path = "/run/oxiroute/backend.sock" },
    { type = "unix", path = "/run//oxiroute///backend.sock" },
  } },
} }"#;
    assert!(matches!(
        error_from(duplicates),
        ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "unix"
    ));
}

#[test]
fn rejects_duplicate_normalized_socket_and_dns_endpoints() {
    for endpoints in [
        r#"{ type = "socket", address = "127.0.0.1:3000" },
    { type = "socket", address = "[::ffff:127.0.0.1]:3000" }"#,
        r#"{ type = "dns", host = "BACKEND.EXAMPLE.TEST", port = 3000 },
    { type = "dns", host = "backend.example.test", port = 3000 }"#,
    ] {
        let source = format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "duplicates", endpoints = {{ {endpoints} }} }},
}} }}"#
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "duplicates"
        ));
    }
}

#[test]
fn rejects_duplicate_normalized_unix_listener_binds() {
    let source = r#"
return {
  version = 1,
  listeners = {
    { name = "first", bind = { type = "unix", path = "/run/oxiroute/live.sock" }, protocol = "rtmp", service = "live" },
    { name = "second", bind = { type = "unix", path = "/run//oxiroute///live.sock" }, protocol = "rtmp", service = "live" },
  },
  rtmp_services = { { name = "live", applications = { { name = "live" } } } },
}
"#;
    assert!(matches!(
        error_from(source),
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "first" && second_name == "second"
    ));
}

#[test]
fn limits_tls_and_health_checks_to_supported_endpoint_transports() {
    let unix_listener = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
        r#"      bind = { type = "unix", path = "/run/oxiroute/web.sock" },"#,
    );
    assert!(matches!(
        error_from(&unix_listener),
        ConfigError::UnsupportedUnixListenerTls { listener, profile }
            if listener == "web" && profile == "web-tls"
    ));

    let unix_pool = |extra: &str| {
        format!(
            r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{
    name = "unix",
    endpoints = {{ {{ type = "unix", path = "/run/oxiroute/backend.sock" }} }},
    {extra}
  }},
}} }}"#
        )
    };
    assert!(matches!(
        error_from(&unix_pool(r#"tls = { server_name = "backend.example.test" },"#)),
        ConfigError::UnsupportedUnixUpstreamTls { pool } if pool == "unix"
    ));
    for health_check in [
        r#"health_check = { type = "tcp" },"#,
        r#"health_check = { type = "http", host = "backend", path = "/healthz" },"#,
    ] {
        assert!(matches!(
            error_from(&unix_pool(health_check)),
            ConfigError::UnsupportedUnixHealthCheck { pool } if pool == "unix"
        ));
    }

    for endpoint in [
        r#"{ type = "socket", address = "127.0.0.1:3000" }"#,
        r#"{ type = "dns", host = "backend.example.test", port = 3000 }"#,
    ] {
        for health_check in [
            r#"{ type = "tcp" }"#,
            r#"{ type = "http", host = "backend.example.test", path = "/healthz" }"#,
        ] {
            let source = format!(
                r#"return {{ version = 1, listeners = {{}}, upstream_pools = {{
  {{ name = "supported", endpoints = {{ {endpoint} }}, health_check = {health_check} }},
}} }}"#
            );
            load_lua(&source).expect("socket and DNS health checks are supported");
        }
    }
}

#[test]
fn management_exposure_checks_only_socket_upstreams() {
    let source = r#"
return {
  version = 1,
  management = { bind = "127.0.0.1:9080" },
  listeners = {},
  upstream_pools = {
    {
      name = "non-socket",
      endpoints = {
        { type = "dns", host = "localhost", port = 9080 },
        { type = "unix", path = "/run/oxiroute/management.sock" },
      },
    },
  },
}
"#;
    load_lua(source).expect("DNS is not resolved and Unix has a distinct identity");
}

#[test]
fn accepts_certificate_and_tls_profile_cardinality_boundaries() {
    let certificates = (0..256)
        .map(|index| {
            format!(
                r#"{{
      name = "certificate-{index}",
      dns_names = {{ "certificate-{index}.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/chain-{index}.pem",
        private_key_path = "/etc/oxiroute/key-{index}.pem",
      }},
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source =
        format!("return {{ version = 1, listeners = {{}}, certificates = {{ {certificates} }} }}");
    let config = load_lua(&source).expect("256 certificates");
    assert_eq!(config.certificates.len(), 256);

    let profiles = (0..256)
        .map(|index| {
            format!(
                r#"{{ name = "profile-{index}", certificates = {{ "shared" }}, default_certificate = "shared" }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  certificates = {{
    {{
      name = "shared",
      dns_names = {{ "shared.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/shared-chain.pem",
        private_key_path = "/etc/oxiroute/shared-key.pem",
      }},
    }},
  }},
  tls_profiles = {{ {profiles} }},
}}"#
    );
    let config = load_lua(&source).expect("256 TLS profiles");
    assert_eq!(config.tls_profiles.len(), 256);
}

#[test]
fn rejects_excessive_certificate_and_tls_profile_cardinality() {
    let certificates = (0..257)
        .map(|index| {
            format!(
                r#"{{
      name = "certificate-{index}",
      dns_names = {{ "certificate-{index}.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/chain-{index}.pem",
        private_key_path = "/etc/oxiroute/key-{index}.pem",
      }},
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source =
        format!("return {{ version = 1, listeners = {{}}, certificates = {{ {certificates} }} }}");
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyCertificates
    ));

    let profiles = (0..257)
        .map(|index| {
            format!(
                r#"{{ name = "profile-{index}", certificates = {{ "shared" }}, default_certificate = "shared" }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  certificates = {{
    {{
      name = "shared",
      dns_names = {{ "shared.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/shared-chain.pem",
        private_key_path = "/etc/oxiroute/shared-key.pem",
      }},
    }},
  }},
  tls_profiles = {{ {profiles} }},
}}"#
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyTlsProfiles
    ));
}

#[test]
fn validates_and_normalizes_certificate_dns_names() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },\n",
        "",
    );
    let error = error_from(&source);
    assert!(matches!(error, ConfigError::Lua(_)));
    assert!(error.to_string().contains("missing field `dns_names`"));

    let names = (0..100)
        .map(|index| format!(r#""HOST-{index}.EXAMPLE.TEST""#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        &format!("      dns_names = {{ {names} }},"),
    );
    let config = load_lua(&source).expect("100 unique certificate DNS names");
    assert_eq!(config.certificates[0].dns_names.len(), 100);
    assert_eq!(config.certificates[0].dns_names[0], "host-0.example.test");

    let too_many = (0..101)
        .map(|index| format!(r#""host-{index}.example.test""#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        &format!("      dns_names = {{ {too_many} }},"),
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyCertificateDnsNames { certificate }
            if certificate == "web-certificate"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = {},",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::EmptyCertificateDnsNames { certificate }
            if certificate == "web-certificate"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"www.example.test\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName {
            certificate,
            dns_name
        } if certificate == "web-certificate" && dns_name == "www.example.test"
    ));

    for dns_name in [
        "",
        "example.test.",
        "caf\u{e9}.example.test",
        "-api.example.test",
        "api-.example.test",
        "api..example.test",
        "api_example.test",
        "*",
        "api.*.example.test",
        "www*.example.test",
        "*.127.0.0.1",
    ] {
        let source = changed(
            "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
            &format!("      dns_names = {{ \"{dns_name}\" }},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidCertificateDnsName { certificate, .. }
                if certificate == "web-certificate"
        ));
    }
}

#[test]
fn validates_and_canonicalizes_certificate_ip_identities() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"192.0.2.10\", \"2001:0DB8:0:0:0:0:0:1\" },",
    );
    let config = load_lua(&source).expect("IPv4 and IPv6 certificate identities");
    assert_eq!(
        config.certificates[0].dns_names,
        ["192.0.2.10", "2001:db8::1"]
    );

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"2001:db8::1\", \"2001:0DB8:0:0:0:0:0:1\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName { dns_name, .. }
            if dns_name == "2001:db8::1"
    ));

    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"192.0.2.10\", \"::ffff:c000:020a\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificateDnsName { dns_name, .. }
            if dns_name == "192.0.2.10"
    ));
}

#[test]
fn validates_certificate_file_paths_lexically() {
    let path = format!("/{}", "a".repeat(4_095));
    let source = changed(
        "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
        &format!("        certificate_chain_path = \"{path}\","),
    );
    load_lua(&source).expect("4096-byte path");

    let too_long = format!("/{}", "a".repeat(4_096));
    let source = changed(
        "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
        &format!("        certificate_chain_path = \"{too_long}\","),
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::InvalidFilePath {
            kind: "certificate",
            field: "source.certificate_chain_path",
            ..
        }
    ));

    for path in [
        "etc/oxiroute/cert.pem",
        "/",
        "/etc//cert.pem",
        "/etc/certs/",
        "/etc/./cert.pem",
        "/etc/../cert.pem",
        r"/etc/\0cert.pem",
    ] {
        let source = changed(
            "        certificate_chain_path = \"/etc/oxiroute/web-chain.pem\",",
            &format!("        certificate_chain_path = \"{path}\","),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidFilePath {
                kind: "certificate",
                field: "source.certificate_chain_path",
                ..
            }
        ));
    }

    let source = changed(
        "        private_key_path = \"/etc/oxiroute/web-key.pem\",",
        "        private_key_path = \"/etc/oxiroute/web-chain.pem\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateCertificatePaths { certificate }
            if certificate == "web-certificate"
    ));
}

#[test]
fn loads_the_canonical_certbot_certificate_source() {
    let source = with_certbot_source(
        "/etc/letsencrypt/live/www.example.test",
        "/etc/letsencrypt/archive/www.example.test",
    );
    let config = load_lua(&source).expect("canonical Certbot certificate source");

    assert_eq!(
        config.certificates[0].source,
        CertificateSource::Certbot {
            live_directory_path: "/etc/letsencrypt/live/www.example.test".into(),
            archive_directory_path: "/etc/letsencrypt/archive/www.example.test".into(),
        }
    );
}

#[test]
fn validates_certbot_directory_paths_lexically() {
    let boundary = format!("/{}", "a".repeat(4_095));
    load_lua(&with_certbot_source(
        &boundary,
        "/etc/letsencrypt/archive/www.example.test",
    ))
    .expect("4096-byte Certbot live directory path");
    load_lua(&with_certbot_source(
        "/etc/letsencrypt/live/www.example.test",
        &boundary,
    ))
    .expect("4096-byte Certbot archive directory path");

    for (field, path) in [
        (
            "source.live_directory_path",
            "etc/letsencrypt/live/name".into(),
        ),
        ("source.live_directory_path", "/".into()),
        ("source.live_directory_path", "/etc//live/name".into()),
        ("source.live_directory_path", "/etc/live/name/".into()),
        ("source.live_directory_path", "/etc/./live/name".into()),
        ("source.live_directory_path", "/etc/../live/name".into()),
        ("source.live_directory_path", r"/etc/live/\0name".into()),
        (
            "source.live_directory_path",
            format!("/{}", "a".repeat(4_096)),
        ),
        (
            "source.archive_directory_path",
            "etc/letsencrypt/archive/name".into(),
        ),
        ("source.archive_directory_path", "/".into()),
        ("source.archive_directory_path", "/etc//archive/name".into()),
        ("source.archive_directory_path", "/etc/archive/name/".into()),
        (
            "source.archive_directory_path",
            "/etc/./archive/name".into(),
        ),
        (
            "source.archive_directory_path",
            "/etc/../archive/name".into(),
        ),
        (
            "source.archive_directory_path",
            r"/etc/archive/\0name".into(),
        ),
        (
            "source.archive_directory_path",
            format!("/{}", "a".repeat(4_096)),
        ),
    ] {
        let (live, archive) = if field == "source.live_directory_path" {
            (path.as_str(), "/etc/letsencrypt/archive/name")
        } else {
            ("/etc/letsencrypt/live/name", path.as_str())
        };
        assert!(matches!(
            error_from(&with_certbot_source(live, archive)),
            ConfigError::InvalidFilePath {
                kind: "certificate",
                field: actual_field,
                ..
            } if actual_field == field
        ));
    }

    assert!(matches!(
        error_from(&with_certbot_source(
            "/etc/letsencrypt/lineage/name",
            "/etc/letsencrypt/lineage/name",
        )),
        ConfigError::DuplicateCertbotDirectories { certificate }
            if certificate == "web-certificate"
    ));
}

#[test]
fn rejects_incomplete_or_noncanonical_certbot_source_objects() {
    for source in [
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        live_directory_path = \"/etc/letsencrypt/live/name\",\n",
            "",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",\n",
            "",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",",
            "        archive_directory_path = \"/etc/letsencrypt/archive/name\",\n        unexpected = true,",
        ),
        with_certbot_source(
            "/etc/letsencrypt/live/name",
            "/etc/letsencrypt/archive/name",
        )
        .replace(
            "        live_directory_path = \"/etc/letsencrypt/live/name\",",
            "        live_directory_path = \"/etc/letsencrypt/live/name\",\n        certificate_chain_path = \"/etc/oxiroute/chain.pem\",",
        ),
    ] {
        assert!(matches!(error_from(&source), ConfigError::Lua(_)));
    }
}

#[test]
fn accepts_only_the_supported_alpn_policies() {
    for (policy, expected) in [
        (r#"{ "http/1.1" }"#, vec![AlpnProtocol::Http11]),
        (r#"{ "h2" }"#, vec![AlpnProtocol::H2]),
        (
            r#"{ "h2", "http/1.1" }"#,
            vec![AlpnProtocol::H2, AlpnProtocol::Http11],
        ),
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {policy},"),
        );
        let config = load_lua(&source).expect("supported ALPN policy");
        assert_eq!(config.tls_profiles[0].alpn, expected);
    }

    for policy in [
        r"{}",
        r#"{ "h2", "h2" }"#,
        r#"{ "http/1.1", "http/1.1" }"#,
        r#"{ "http/1.1", "h2" }"#,
        r#"{ "h2", "http/1.1", "h2" }"#,
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {policy},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidTlsProfileAlpn { profile } if profile == "web-tls"
        ));
    }
}

#[test]
fn validates_tls_policy_bounds_and_paths() {
    for (field, policy) in [
        (
            "cipher_list",
            r#"{ cipher_list = "", session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_cache.name",
            r#"{ session_cache = { name = "invalid:name", size_bytes = 10485760 }, session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_cache.size_bytes",
            r#"{ session_cache = { name = "SSL", size_bytes = 255 }, session_tickets = false, prefer_server_ciphers = true }"#,
        ),
        (
            "session_timeout_seconds",
            r"{ session_timeout_seconds = 0, session_tickets = false, prefer_server_ciphers = true }",
        ),
    ] {
        let source = changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            &format!("      alpn = {{ \"h2\", \"http/1.1\" }},\n      policy = {policy},"),
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidTlsProfilePolicy {
                profile,
                field: invalid_field,
                ..
            } if profile == "web-tls" && invalid_field == field
        ));
    }

    let source = changed(
        "      alpn = { \"h2\", \"http/1.1\" },",
        "      alpn = { \"h2\", \"http/1.1\" },\n      policy = { dh_parameters_path = \"relative.pem\", session_tickets = false, prefer_server_ciphers = true },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::InvalidFilePath {
            kind: "TLS profile",
            name,
            field: "policy.dh_parameters_path",
            ..
        } if name == "web-tls"
    ));
}

#[test]
fn validates_tls_profile_and_listener_references() {
    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"missing\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::UnknownTlsProfileCertificate {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "missing"
    ));

    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = {},",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::EmptyTlsProfileCertificates { profile } if profile == "web-tls"
    ));

    let source = changed(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"web-certificate\", \"web-certificate\" },",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::DuplicateTlsProfileCertificate {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "web-certificate"
    ));

    let source = changed(
        "      default_certificate = \"web-certificate\",",
        "      default_certificate = \"missing\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TlsProfileDefaultNotListed {
            profile,
            certificate
        } if profile == "web-tls" && certificate == "missing"
    ));

    let source = changed(
        "      tls_profile = \"web-tls\",",
        "      tls_profile = \"missing\",",
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::UnknownListenerTlsProfile { listener, profile }
            if listener == "web" && profile == "missing"
    ));

    for (fragment, listener, protocol) in [
        (
            "      service = \"database\",\n      max_connections = 1000,",
            "database",
            Protocol::Tcp,
        ),
        (
            "      protocol = \"rtmp\",\n      service = \"live\",\n      max_connections = 500,",
            "live",
            Protocol::Rtmp,
        ),
    ] {
        let replacement = fragment.replace(
            "      max_connections",
            "      tls_profile = \"web-tls\",\n      max_connections",
        );
        let source = changed(fragment, &replacement);
        assert!(matches!(
            error_from(&source),
            ConfigError::UnexpectedListenerTlsProfile {
                listener: actual_listener,
                protocol: actual_protocol,
                profile,
            } if actual_listener == listener
                && actual_protocol == protocol
                && profile == "web-tls"
        ));
    }
}

#[test]
fn rejects_dns_name_ownership_overlap_within_a_tls_profile() {
    for dns_name in ["www.example.test", "*.example.test"] {
        let source = changed(
            "    },\n  },\n  tls_profiles = {",
            &format!(
                r#"    }},
    {{
      name = "overlapping-certificate",
      dns_names = {{ "{dns_name}" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/overlapping-chain.pem",
        private_key_path = "/etc/oxiroute/overlapping-key.pem",
      }},
    }},
  }},
  tls_profiles = {{"#
            ),
        );
        let source = source.replace(
            "      certificates = { \"web-certificate\" },",
            "      certificates = { \"web-certificate\", \"overlapping-certificate\" },",
        );
        assert!(matches!(
            error_from(&source),
            ConfigError::OverlappingTlsProfileDnsName {
                profile,
                dns_name: actual_dns_name,
                first_certificate,
                second_certificate,
            } if profile == "web-tls"
                && actual_dns_name == dns_name
                && first_certificate == "web-certificate"
                && second_certificate == "overlapping-certificate"
        ));
    }
}

#[test]
fn permits_shared_canonical_ip_identities_within_a_tls_profile() {
    let source = changed(
        "      dns_names = { \"WWW.EXAMPLE.TEST\", \"*.EXAMPLE.TEST\" },",
        "      dns_names = { \"::ffff:c000:020a\" },",
    );
    let source = source.replace(
        "    },\n  },\n  tls_profiles = {",
        r#"    },
    {
      name = "ip-certificate",
      dns_names = { "192.0.2.10" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/ip-chain.pem",
        private_key_path = "/etc/oxiroute/ip-key.pem",
      },
    },
  },
  tls_profiles = {"#,
    );
    let source = source.replace(
        "      certificates = { \"web-certificate\" },",
        "      certificates = { \"web-certificate\", \"ip-certificate\" },",
    );

    let config = load_lua(&source).expect("shared IP identities do not participate in SNI");
    assert_eq!(config.certificates[0].dns_names, ["192.0.2.10"]);
    assert_eq!(config.certificates[1].dns_names, ["192.0.2.10"]);
}

#[test]
fn loads_a_bounded_http_retry_budget() {
    for max_retries in [1, 2, 3] {
        let source = changed(
            "            policy = {},",
            &format!("            policy = {{ retry = {{ max_retries = {max_retries} }} }},"),
        );
        let config = load_lua(&source).expect("bounded retry budget");

        assert_eq!(
            serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["action"]
                ["policy"]["retry"]["max_retries"],
            max_retries
        );
    }
}

#[test]
fn rejects_an_excessive_http_retry_budget() {
    let source = changed(
        "            policy = {},",
        "            policy = { retry = { max_retries = 4 } },",
    );
    let error = error_from(&source);

    assert!(matches!(
        error,
        ConfigError::InvalidHttpRoute {
            service,
            route: 0,
            field: "action.policy.retry.max_retries",
            ..
        } if service == "web"
    ));
}

#[test]
fn loads_tcp_and_http_health_check_policies() {
    let tcp_source = with_web_pool_fields("      health_check = { type = \"tcp\" },");
    let tcp = load_lua(&tcp_source).expect("TCP health check");
    let tcp_check = tcp.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("TCP policy");
    assert_eq!(tcp_check.kind, HealthCheckType::Tcp);
    assert_eq!(tcp_check.interval_ms, 10_000);
    assert_eq!(tcp_check.timeout_ms, 1_000);
    assert_eq!(tcp_check.healthy_threshold, 1);
    assert_eq!(tcp_check.unhealthy_threshold, 3);

    let http_source = with_web_pool_fields(
        r#"      health_check = {
        type = "http",
        interval_ms = 5000,
        timeout_ms = 500,
        healthy_threshold = 2,
        unhealthy_threshold = 4,
        host = "backend.internal:3000",
        path = "/healthz",
      },"#,
    );
    let http = load_lua(&http_source).expect("HTTP health check");
    let http_check = http.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("HTTP policy");
    assert_eq!(http_check.kind, HealthCheckType::Http);
    assert_eq!(http_check.interval_ms, 5_000);
    assert_eq!(http_check.timeout_ms, 500);
    assert_eq!(http_check.healthy_threshold, 2);
    assert_eq!(http_check.unhealthy_threshold, 4);
    assert_eq!(http_check.host.as_deref(), Some("backend.internal:3000"));
    assert_eq!(http_check.path.as_deref(), Some("/healthz"));
}

#[test]
fn loads_upstream_tls_and_all_supported_http_version_ranges() {
    for (min, max) in [("1.1", "1.1"), ("1.1", "2"), ("2", "2")] {
        let fields = format!(
            "{}\n      http_versions = {{ min = \"{min}\", max = \"{max}\" }},",
            upstream_tls("BACKEND.EXAMPLE.COM")
        );
        let source = with_web_pool_fields(&fields);
        let config = load_lua(&source).expect("supported upstream HTTP version range");
        let pool = &config.upstream_pools[0];
        let tls = pool.tls.as_ref().expect("upstream TLS");

        assert_eq!(tls.server_name, "backend.example.com");
        assert_eq!(
            tls.ca_certificate_path.as_deref(),
            Some(std::path::Path::new("/etc/oxiroute/upstream-ca.pem"))
        );
        assert_eq!(
            pool.http_versions.min,
            if min == "1.1" {
                HttpVersion::Http11
            } else {
                HttpVersion::Http2
            }
        );
        assert_eq!(
            pool.http_versions.max,
            if max == "1.1" {
                HttpVersion::Http11
            } else {
                HttpVersion::Http2
            }
        );
    }

    let fields = format!(
        "{}\n      http_versions = {{ min = \"3\", max = \"3\" }},",
        upstream_tls("BACKEND.EXAMPLE.COM")
    );
    let source = with_web_pool_fields(&fields)
        .replacen(r#"alpn = { "h2", "http/1.1" },"#, r#"alpn = { "h3" },"#, 1)
        .replacen(
            r#"bind = { type = "socket", address = "127.0.0.1:8080" },"#,
            r#"bind = { type = "udp", address = "127.0.0.1:8080" },"#,
            1,
        )
        .replacen(r#"protocol = "http","#, r#"protocol = "http3","#, 1)
        .replacen(
            r#"path = { kind = "segment_prefix", value = "/api" },"#,
            r#"path = { kind = "segment_prefix", value = "/api" },
          policy = { request_buffering = true },"#,
            1,
        );
    let config = load_lua(&source).expect("HTTP/3 upstream range");
    assert_eq!(
        config.upstream_pools[0].http_versions.min,
        HttpVersion::Http3
    );
    assert_eq!(
        config.upstream_pools[0].http_versions.max,
        HttpVersion::Http3
    );
}

#[test]
fn validates_upstream_ca_paths_lexically() {
    let path = format!("/{}", "a".repeat(4_095));
    let fields = format!(
        r#"      tls = {{ server_name = "backend.example.com", ca_certificate_path = "{path}" }},"#
    );
    load_lua(&with_web_pool_fields(&fields)).expect("4096-byte upstream CA path");

    for path in [
        format!("/{}", "a".repeat(4_096)),
        "etc/oxiroute/ca.pem".into(),
        "/etc//ca.pem".into(),
        "/etc/ca/".into(),
        "/etc/./ca.pem".into(),
        "/etc/../ca.pem".into(),
        r"/etc/\0ca.pem".into(),
    ] {
        let fields = format!(
            r#"      tls = {{ server_name = "backend.example.com", ca_certificate_path = "{path}" }},"#
        );
        assert!(matches!(
            error_from(&with_web_pool_fields(&fields)),
            ConfigError::InvalidFilePath {
                kind: "upstream pool",
                field: "tls.ca_certificate_path",
                ..
            }
        ));
    }
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_file_paths() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

    use oxiroute_config::{
        DnsResolutionPolicy, HttpVersionPolicy, UpstreamConnectionReuse, UpstreamPool,
        UpstreamServer, UpstreamTls, validate_upstream_pool_definitions,
    };

    let pool = UpstreamPool {
        name: "secure".into(),
        servers: vec![UpstreamServer {
            name: "secure-1".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:443".parse().expect("endpoint"),
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        tls: Some(UpstreamTls {
            server_name: "backend.example.com".into(),
            ca_certificate_path: Some(PathBuf::from(OsString::from_vec(
                b"/etc/oxiroute/ca-\xff.pem".to_vec(),
            ))),
        }),
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::Safe,
    };

    assert!(matches!(
        validate_upstream_pool_definitions(&[pool], None),
        Err(ConfigError::InvalidFilePath {
            kind: "upstream pool",
            field: "tls.ca_certificate_path",
            detail: "path must be valid UTF-8",
            ..
        })
    ));
}

#[test]
fn validates_upstream_tls_server_names() {
    let boundary_name = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    let config = load_lua(&with_web_pool_fields(&upstream_tls(&boundary_name)))
        .expect("253-byte DNS server name");
    assert_eq!(
        config.upstream_pools[0]
            .tls
            .as_ref()
            .expect("upstream TLS")
            .server_name,
        boundary_name
    );

    let too_long = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    for server_name in [
        String::new(),
        "*.example.com".into(),
        "127.0.0.1".into(),
        "::1".into(),
        "example.com.".into(),
        "-api.example.com".into(),
        "api-.example.com".into(),
        "api..example.com".into(),
        "caf\u{e9}.example.com".into(),
        format!("{}.example.com", "a".repeat(64)),
        too_long,
    ] {
        assert!(matches!(
            error_from(&with_web_pool_fields(&upstream_tls(&server_name))),
            ConfigError::InvalidUpstreamTlsServerName { pool, .. }
                if pool == "web-backends"
        ));
    }
}

#[test]
fn rejects_invalid_or_plaintext_http2_upstream_ranges() {
    let fields = format!(
        "{}\n      http_versions = {{ min = \"2\", max = \"1.1\" }},",
        upstream_tls("backend.example.com")
    );
    assert!(matches!(
        error_from(&with_web_pool_fields(&fields)),
        ConfigError::InvalidHttpVersionRange {
            pool,
            min: "2",
            max: "1.1"
        } if pool == "web-backends"
    ));

    for min in ["1.1", "2"] {
        let fields = format!("      http_versions = {{ min = \"{min}\", max = \"2\" }},");
        assert!(matches!(
            error_from(&with_web_pool_fields(&fields)),
            ConfigError::H2RequiresUpstreamTls { pool } if pool == "web-backends"
        ));
    }

    let fields = r#"      http_versions = { min = "3", max = "3" },"#;
    assert!(matches!(
        error_from(&with_web_pool_fields(fields)),
        ConfigError::H3RequiresUpstreamTls { pool } if pool == "web-backends"
    ));
    let fields = format!(
        "{}\n      http_versions = {{ min = \"2\", max = \"3\" }},",
        upstream_tls("backend.example.com")
    );
    assert!(matches!(
        error_from(&with_web_pool_fields(&fields)),
        ConfigError::InvalidHttpVersionRange {
            pool,
            min: "2",
            max: "3"
        } if pool == "web-backends"
    ));
}

#[test]
fn rejects_health_checks_combined_with_upstream_tls() {
    let fields = format!(
        "{}\n      health_check = {{ type = \"tcp\" }},",
        upstream_tls("backend.example.com")
    );
    let error = error_from(&with_web_pool_fields(&fields));

    assert!(matches!(
        &error,
        ConfigError::UnsupportedTlsHealthCheck { pool } if pool == "web-backends"
    ));
    assert!(
        error
            .to_string()
            .contains("combines `health_check` with `tls`, which is not supported")
    );
}

#[test]
fn rejects_l4_references_to_tls_enabled_upstream_pools() {
    let pool = r#"      endpoints = { { type = "socket", address = "10.0.0.12:5432" } },
      algorithm = "round_robin","#;
    let source = changed(
        pool,
        &format!("{pool}\n      tls = {{ server_name = \"database.example.com\" }},"),
    );

    assert!(matches!(
        error_from(&source),
        ConfigError::TlsUpstreamPoolForL4Service { service, pool }
            if service == "database" && pool == "database-backends"
    ));
}

#[test]
fn rejects_invalid_health_check_timing_and_thresholds() {
    for policy in [
        r#"{ type = "tcp", interval_ms = 999 }"#,
        r#"{ type = "tcp", interval_ms = 86400001 }"#,
        r#"{ type = "tcp", interval_ms = 40000, timeout_ms = 30001 }"#,
        r#"{ type = "tcp", healthy_threshold = 0 }"#,
        r#"{ type = "tcp", healthy_threshold = 101 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 0 }"#,
        r#"{ type = "tcp", unhealthy_threshold = 101 }"#,
    ] {
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn health_check_timeout_may_equal_its_interval() {
    let source = with_web_pool_fields(
        r#"      health_check = { type = "tcp", interval_ms = 10000, timeout_ms = 10000 },"#,
    );
    let config = load_lua(&source).expect("equal health interval and timeout");
    let health = config.upstream_pools[0]
        .health_check
        .as_ref()
        .expect("health check");

    assert_eq!(health.interval_ms, 10_000);
    assert_eq!(health.timeout_ms, 10_000);
}

#[test]
fn rejects_health_check_fields_that_do_not_match_the_probe_type() {
    load_lua(&with_web_pool_fields(
        r#"      health_check = { type = "http", path = "/healthz" },"#,
    ))
    .expect("HTTP health-check Host policy is optional");

    for policy in [
        r#"{ type = "http", host = "backend.internal" }"#,
        r#"{ type = "http", host = "user@backend.internal", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal:not-a-port", path = "/healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "healthz" }"#,
        r#"{ type = "http", host = "backend.internal", path = "/healthz?full=true" }"#,
        r#"{ type = "tcp", host = "backend.internal" }"#,
        r#"{ type = "tcp", path = "/healthz" }"#,
    ] {
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }

    for policy in [
        format!(
            r#"{{ type = "http", host = "{}", path = "/healthz" }}"#,
            "a".repeat(256)
        ),
        format!(
            r#"{{ type = "http", host = "backend.internal", path = "/{}" }}"#,
            "a".repeat(2_048)
        ),
    ] {
        let source = with_web_pool_fields(&format!("      health_check = {policy},"));
        assert!(matches!(
            error_from(&source),
            ConfigError::InvalidHealthCheck { pool, .. } if pool == "web-backends"
        ));
    }
}

#[test]
fn normalizes_exact_wildcard_and_ip_hosts() {
    let source = changed(
        WEB_ROUTES,
        r#"      routes = {
        {
          host = { kind = "normalized_host", value = "EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
        {
          host = { kind = "normalized_host", value = "*.API.EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
        {
          host = { kind = "normalized_host", value = "2001:0DB8:0:0:0:0:0:1" },
          path = { kind = "segment_prefix", value = "/" },
          action = { type = "proxy", upstream_pool = "web-backends", policy = {} },
        },
      },"#,
    );
    let config = load_lua(&source).expect("valid host matchers");
    let routes = &config.http_services[0].routes;

    let routes = serde_json::to_value(routes).expect("serialized routes");
    assert_eq!(routes[0]["host"]["value"], "example.com");
    assert_eq!(routes[1]["host"]["value"], "*.api.example.com");
    assert_eq!(routes[2]["host"]["value"], "2001:db8::1");
}

#[test]
fn preserves_path_selector_semantics_before_duplicate_detection() {
    let source = changed(
        "          path = { kind = \"segment_prefix\", value = \"/api\" },",
        "          path = { kind = \"segment_prefix\", value = \"/api/\" },",
    );
    let config = load_lua(&source).expect("normalized path prefix");

    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["path"]
            ["value"],
        "/api/"
    );

    let duplicate = changed(
        "        },\n      },\n      upstream_io_timeout_ms",
        r#"        },
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api/" },
          methods = { "POST", "GET" },
          action = {
            type = "proxy",
            upstream_pool = "database-backends",
            policy = {},
          },
        },
      },
      upstream_io_timeout_ms"#,
    );
    load_lua(&duplicate).expect("trailing slash retains distinct segment-prefix semantics");
}

#[test]
fn canonicalizes_percent_triplet_case_in_route_prefixes() {
    let source = changed(
        "          path = { kind = \"segment_prefix\", value = \"/api\" },",
        "          path = { kind = \"segment_prefix\", value = \"/api%3azone\" },",
    );
    let config = load_lua(&source).expect("canonical percent triplet");

    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["path"]
            ["value"],
        "/api%3Azone"
    );
}

#[test]
fn rejects_unsupported_versions() {
    let error = error_from(&changed("  version = 1,", "  version = 2,"));

    assert!(matches!(error, ConfigError::UnsupportedVersion(2)));
}

#[test]
fn rejects_non_loopback_and_zero_port_management_binds() {
    let error = error_from(&changed("127.0.0.1:9080", "0.0.0.0:9080"));
    assert!(matches!(error, ConfigError::ManagementMustUseLoopback(_)));

    let error = error_from(&changed("127.0.0.1:9080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "management listener",
            name,
            field: "bind"
        } if name == "management"
    ));
}

#[test]
fn rejects_blank_names_in_every_namespace() {
    let cases = [
        (
            "      name = \"web-certificate\",\n      dns_names",
            "      name = \"  \",\n      dns_names",
            "certificate",
        ),
        (
            "      name = \"web-tls\",\n      certificate",
            "      name = \"  \",\n      certificate",
            "TLS profile",
        ),
        (
            "      name = \"web\",\n      bind",
            "      name = \"  \",\n      bind",
            "listener",
        ),
        (
            "      name = \"web-backends\",",
            "      name = \"  \",",
            "upstream pool",
        ),
        (
            "      name = \"web\",\n      routes",
            "      name = \"  \",\n      routes",
            "HTTP service",
        ),
        (
            "      name = \"database\",\n      upstream_pool",
            "      name = \"  \",\n      upstream_pool",
            "L4 service",
        ),
    ];

    for (from, to, expected_namespace) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::BlankName { namespace, index: 0 }
                if namespace == expected_namespace
        ));
    }
}

#[test]
fn rejects_names_with_surrounding_whitespace_or_control_characters() {
    for name in [" web ", "web\\nedge"] {
        let error = error_from(&changed(
            "      name = \"web\",\n      bind",
            &format!("      name = \"{name}\",\n      bind"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidName {
                namespace: "listener",
                index: 0,
                ..
            }
        ));
    }
}

#[test]
fn rejects_duplicate_names_in_every_namespace() {
    let source = changed(
        "  },\n  tls_profiles = {",
        r#"    {
      name = "web-certificate",
      dns_names = { "duplicate.example.test" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/duplicate-chain.pem",
        private_key_path = "/etc/oxiroute/duplicate-key.pem",
      },
    },
  },
  tls_profiles = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "certificate", name }
            if name == "web-certificate"
    ));

    let source = changed(
        "  },\n  listeners = {",
        r#"    {
      name = "web-tls",
      certificates = { "web-certificate" },
      default_certificate = "web-certificate",
    },
  },
  listeners = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "TLS profile", name } if name == "web-tls"
    ));

    let error = error_from(&changed(
        "      name = \"database\",\n      bind",
        "      name = \"web\",\n      bind",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "listener", name } if name == "web"
    ));

    let error = error_from(&changed(
        "      name = \"database-backends\",",
        "      name = \"web-backends\",",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "upstream pool", name }
            if name == "web-backends"
    ));

    let source = changed(
        "      max_request_body_bytes = 2097152,\n    },\n  },\n  rtmp_services = {",
        r#"      max_request_body_bytes = 2097152,
    },
    {
      name = "web",
      routes = {
        {
          path = { kind = "exact", value = "/duplicate" },
          action = { type = "fixed_response", status = 200 },
        },
      },
    },
  },
  rtmp_services = {"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "HTTP service", name } if name == "web"
    ));

    let source = changed(
        "      lifetime_timeout_ms = 600000,\n    },",
        r#"      lifetime_timeout_ms = 600000,
    },
    {
      name = "database",
      upstream_pool = "database-backends",
    },"#,
    );
    let error = error_from(&source);
    assert!(matches!(
        error,
        ConfigError::DuplicateName { namespace: "L4 service", name } if name == "database"
    ));
}

#[test]
fn rejects_overlapping_and_zero_port_listener_binds() {
    let error = error_from(&changed("127.0.0.1:15432", "127.0.0.1:8080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind {
            first_name,
            second_name,
            ..
        } if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "0.0.0.0:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:15432"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "web" && second_name == "database"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "[::ffff:127.0.0.1]:9080"));
    assert!(matches!(
        error,
        ConfigError::OverlappingBind { first_name, second_name, .. }
            if first_name == "management" && second_name == "web"
    ));

    let error = error_from(&changed("127.0.0.1:8080", "127.0.0.1:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "listener",
            name,
            field: "bind"
        } if name == "web"
    ));
}

#[test]
fn rejects_zero_listener_connection_limits() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 0,",
    ));

    assert!(matches!(
        error,
        ConfigError::ZeroLimit {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn rejects_listener_limits_that_json_cannot_represent_exactly() {
    let error = error_from(&changed(
        "      max_connections = 5000,",
        "      max_connections = 9007199254740992,",
    ));

    assert!(matches!(
        error,
        ConfigError::LimitTooLarge {
            kind: "listener",
            name,
            field: "max_connections"
        } if name == "web"
    ));
}

#[test]
fn requires_every_listener_to_reference_a_same_kind_service() {
    let cases = [
        ("      service = \"web\",\n", Protocol::Http, "web"),
        ("      service = \"database\",\n", Protocol::Tcp, "database"),
        ("      service = \"live\",\n", Protocol::Rtmp, "live"),
    ];

    for (field, protocol, listener) in cases {
        let error = error_from(&changed(field, ""));
        assert!(matches!(
            error,
            ConfigError::MissingListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn requires_listeners_to_reference_same_kind_services() {
    let cases = [
        (
            "      service = \"web\",",
            "      service = \"database\",",
            Protocol::Http,
            "web",
        ),
        (
            "      service = \"database\",",
            "      service = \"web\",",
            Protocol::Tcp,
            "database",
        ),
        (
            "      service = \"live\",",
            "      service = \"web\",",
            Protocol::Rtmp,
            "live",
        ),
    ];

    for (from, to, protocol, listener) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::UnknownListenerService {
                listener: actual_listener,
                protocol: actual_protocol,
                ..
            } if actual_listener == listener && actual_protocol == protocol
        ));
    }
}

#[test]
fn rejects_empty_duplicate_and_zero_port_pool_endpoints() {
    let error = error_from(&changed(
        "      endpoints = { { type = \"socket\", address = \"10.0.0.12:5432\" } },",
        "      endpoints = {},",
    ));
    assert!(matches!(
        error,
        ConfigError::EmptyUpstreamEndpoints { pool } if pool == "database-backends"
    ));

    let error = error_from(&changed(
        "      endpoints = { { type = \"socket\", address = \"10.0.0.12:5432\" } },",
        "      endpoints = { { type = \"socket\", address = \"10.0.0.12:5432\" }, { type = \"socket\", address = \"10.0.0.12:5432\" } },",
    ));
    assert!(matches!(
        error,
        ConfigError::DuplicateUpstreamEndpoint { pool, .. } if pool == "database-backends"
    ));

    let error = error_from(&changed("10.0.0.12:5432", "10.0.0.12:0"));
    assert!(matches!(
        error,
        ConfigError::ZeroPort {
            kind: "upstream pool",
            name,
            field: "endpoints"
        } if name == "database-backends"
    ));
}

#[test]
fn loads_and_round_trips_weighted_round_robin_policy() {
    let config = load_lua(
        r#"return {
  version = 1,
  listeners = {},
  upstream_pools = {
    {
      name = "weighted",
      servers = {
        { name = "primary", endpoint = { type = "socket", address = "127.0.0.1:3000" } },
        { name = "backup", endpoint = { type = "socket", address = "127.0.0.1:3001" } },
      },
      algorithm = { type = "weighted_round_robin", weights = { 3, 1 } },
    },
  },
}"#,
    )
    .expect("weighted round-robin policy");
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::WeightedRoundRobin {
            weights: vec![3, 1]
        }
    );

    let rendered = render_lua(&config).expect("weighted round-robin render");
    assert!(rendered.contains("type = \"weighted_round_robin\""));
    assert_eq!(
        load_lua(&rendered).expect("weighted round-robin reload"),
        config
    );
}

#[test]
fn rejects_weighted_round_robin_with_missing_zero_or_oversized_weights() {
    let sources = [r"{ 3 }", r"{ 3, 0 }", r"{ 3, 101 }"];
    for weights in sources {
        let source = format!(
            r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{
      name = "weighted",
      servers = {{
        {{ name = "primary", endpoint = {{ type = "socket", address = "127.0.0.1:3000" }} }},
        {{ name = "backup", endpoint = {{ type = "socket", address = "127.0.0.1:3001" }} }},
      }},
      algorithm = {{ type = "weighted_round_robin", weights = {weights} }},
    }},
  }},
}}"#
        );
        assert!(matches!(
            load_lua(&source).expect_err("invalid weighted round-robin policy"),
            ConfigError::InvalidUpstreamWeights { pool, .. } if pool == "weighted"
        ));
    }
}

#[test]
fn rejects_excessive_upstream_endpoint_cardinality() {
    let endpoints = (10_000..10_257)
        .map(|port| format!(r#"{{ type = "socket", address = "127.0.0.1:{port}" }}"#))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "oversized", endpoints = {{ {endpoints} }} }},
  }},
}}"#
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyUpstreamEndpoints { pool } if pool == "oversized"
    ));

    let pools = (0..5)
        .map(|pool| {
            let endpoints = (0..205)
                .map(|offset| {
                    let port = 20_000 + pool * 205 + offset;
                    format!(r#"{{ type = "socket", address = "127.0.0.1:{port}" }}"#)
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(r#"{{ name = "pool-{pool}", endpoints = {{ {endpoints} }} }}"#)
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let source = format!(
        r"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {pools}
  }},
}}"
    );
    assert!(matches!(
        error_from(&source),
        ConfigError::TooManyTotalUpstreamEndpoints
    ));
}

#[test]
fn rejects_a_pool_that_exposes_the_management_endpoint() {
    for endpoint in [
        "127.0.0.1:9080",
        "0.0.0.0:9080",
        "[::]:9080",
        "[::ffff:127.0.0.1]:9080",
        "[::ffff:0.0.0.0]:9080",
    ] {
        let error = error_from(&changed("10.0.0.12:5432", endpoint));
        assert!(matches!(
            error,
            ConfigError::ManagementUpstreamEndpoint { pool, .. }
                if pool == "database-backends"
        ));
    }
}

#[test]
fn rejects_empty_http_routes() {
    let source = changed(WEB_ROUTES, "      routes = {},");
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::EmptyHttpRoutes { service } if service == "web"));
}

#[test]
fn rejects_invalid_route_hosts() {
    for host in [
        "",
        "*.127.0.0.1",
        "api.*.example.com",
        "-api.example.com",
        "api..example.com",
    ] {
        let error = error_from(&changed(
            "          host = { kind = \"normalized_host\", value = \"example.com\" },",
            &format!("          host = {{ kind = \"normalized_host\", value = \"{host}\" }},"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute {
                service,
                route: 0,
                field: "host",
                ..
            } if service == "web"
        ));
    }
}

#[test]
fn rejects_invalid_route_path_prefixes() {
    for path_prefix in [
        "api",
        "/api?query",
        "/api#fragment",
        "/api path",
        "/api<internal",
        "/api>internal",
        "/api`internal",
        "/api/../internal",
        "/api//internal",
        "/api%2finternal",
        "/%61pi",
    ] {
        let error = error_from(&changed(
            "          path = { kind = \"segment_prefix\", value = \"/api\" },",
            &format!(
                "          path = {{ kind = \"segment_prefix\", value = \"{path_prefix}\" }},"
            ),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute { service, route: 0, field: "path", .. }
                if service == "web"
        ));
    }
}

#[test]
fn rejects_invalid_and_duplicate_route_methods() {
    for method in ["GE T", "G\u{c9}T", ""] {
        let error = error_from(&changed(
            "          methods = { \"GET\", \"POST\" },",
            &format!("          methods = {{ \"{method}\" }},"),
        ));
        assert!(matches!(
            error,
            ConfigError::InvalidHttpRoute {
                service,
                route: 0,
                field: "methods",
                ..
            } if service == "web"
        ));
    }

    let error = error_from(&changed(
        "          methods = { \"GET\", \"POST\" },",
        "          methods = { \"GET\", \"get\" },",
    ));
    assert!(matches!(
        error,
        ConfigError::InvalidHttpRoute {
            service,
            route: 0,
            field: "methods",
            ..
        } if service == "web"
    ));
}

#[test]
fn rejects_duplicate_equivalent_routes_after_normalization() {
    let source = changed(
        "        },\n      },\n      upstream_io_timeout_ms",
        r#"        },
        {
          host = { kind = "normalized_host", value = "EXAMPLE.COM" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "POST", "GET" },
          action = { type = "fixed_response", status = 200 },
        },
      },
      upstream_io_timeout_ms"#,
    );
    let error = error_from(&source);

    assert!(matches!(
        error,
        ConfigError::DuplicateHttpRoute {
            service,
            first_route: 0,
            duplicate_route: 1
        } if service == "web"
    ));
}

#[test]
fn rejects_unknown_route_and_l4_upstream_pools() {
    let error = error_from(&changed(
        "          upstream_pool = \"web-backends\",",
        "          upstream_pool = \"missing\",",
    ));
    assert!(matches!(
        error,
        ConfigError::UnknownRouteUpstreamPool {
            service,
            route: 0,
            pool
        } if service == "web" && pool == "missing"
    ));

    let error = error_from(&changed(
        "      upstream_pool = \"database-backends\",",
        "      upstream_pool = \"missing\",",
    ));
    assert!(matches!(
        error,
        ConfigError::UnknownL4UpstreamPool { service, pool }
            if service == "database" && pool == "missing"
    ));
}

#[test]
fn rejects_zero_http_service_limits() {
    let cases = [
        (
            "      upstream_io_timeout_ms = 15000,",
            "      upstream_io_timeout_ms = 0,",
            "upstream_io_timeout_ms",
        ),
        (
            "      max_request_body_bytes = 2097152,",
            "      max_request_body_bytes = 0,",
            "max_request_body_bytes",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "HTTP service",
                name,
                field
            } if name == "web" && field == expected_field
        ));
    }
}

#[test]
fn rejects_zero_l4_service_timeouts() {
    let cases = [
        (
            "      connect_timeout_ms = 5000,",
            "      connect_timeout_ms = 0,",
            "connect_timeout_ms",
        ),
        (
            "      idle_timeout_ms = 120000,",
            "      idle_timeout_ms = 0,",
            "idle_timeout_ms",
        ),
        (
            "      lifetime_timeout_ms = 600000,",
            "      lifetime_timeout_ms = 0,",
            "lifetime_timeout_ms",
        ),
    ];

    for (from, to, expected_field) in cases {
        let error = error_from(&changed(from, to));
        assert!(matches!(
            error,
            ConfigError::ZeroLimit {
                kind: "L4 service",
                name,
                field
            } if name == "database" && field == expected_field
        ));
    }
}

#[test]
fn rejects_unknown_fields_including_the_old_direct_upstream() {
    let source = changed(
        "      service = \"web\",",
        "      service = \"web\",\n      upstream = \"127.0.0.1:3000\",",
    );
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::Lua(_)));
    assert!(error.to_string().contains("unknown field `upstream`"));
}

#[test]
fn rejects_unknown_fields_in_tls_and_http_version_objects() {
    let cases = [
        changed(
            "      name = \"web-certificate\",",
            "      name = \"web-certificate\",\n      unexpected = true,",
        ),
        changed(
            "        type = \"files\",",
            "        type = \"files\",\n        unexpected = true,",
        ),
        changed(
            "      name = \"web-tls\",",
            "      name = \"web-tls\",\n      unexpected = true,",
        ),
        with_web_pool_fields(
            r#"      tls = { server_name = "backend.example.com", unexpected = true },"#,
        ),
        with_web_pool_fields(
            r#"      http_versions = { min = "1.1", max = "1.1", unexpected = true },"#,
        ),
    ];

    for source in cases {
        let error = error_from(&source);
        assert!(matches!(error, ConfigError::Lua(_)));
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }
}

#[test]
fn rejects_unknown_tls_and_http_version_values() {
    let cases = [
        changed("        type = \"files\",", "        type = \"pkcs12\","),
        changed(
            "      min_version = \"1.3\",",
            "      min_version = \"1.1\",",
        ),
        changed(
            "      alpn = { \"h2\", \"http/1.1\" },",
            "      alpn = { \"http/2\" },",
        ),
        with_web_pool_fields(r#"      http_versions = { min = "1", max = "1.1" },"#),
        with_web_pool_fields(r#"      http_versions = { min = "1.1", max = "4" },"#),
    ];

    for source in cases {
        assert!(matches!(error_from(&source), ConfigError::Lua(_)));
    }
}

#[test]
fn rejects_unknown_protocols_and_algorithms() {
    let protocol_error = error_from(&changed(
        "      protocol = \"http\",",
        "      protocol = \"sctp\",",
    ));
    assert!(matches!(protocol_error, ConfigError::Lua(_)));

    let algorithm_error = error_from(&changed(
        r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin","#,
        r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "unknown","#,
    ));
    assert!(matches!(algorithm_error, ConfigError::Lua(_)));
}

#[test]
fn does_not_expose_operating_system_functions() {
    let source = r#"
os.execute("touch /tmp/oxiroute-lua-escaped")
return { version = 1, listeners = {} }
"#;
    let error = error_from(source);

    assert!(error.to_string().contains("os"));
    assert!(!std::path::Path::new("/tmp/oxiroute-lua-escaped").exists());
}

#[test]
fn enforces_the_source_size_limit() {
    let source = " ".repeat(1024 * 1024 + 1);
    let error = error_from(&source);

    assert!(matches!(error, ConfigError::SourceTooLarge));
}
