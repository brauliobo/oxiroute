use std::path::{Path, PathBuf};

use oxiroute_config::{
    AlpnProtocol, CertificateSource, HttpHostSelector, HttpPathSelector, HttpRedirectLocation,
    HttpRouteAction, ListenerBind, Protocol, SelfSignedKeyType, TlsVersion, UpstreamAlgorithm,
    UpstreamEndpoint, ValidatedConfig,
};
use oxiroute_config_source::{ConfigFormat, load_lua, resolve_source_with_format};

fn eval_both(script: &str) -> ValidatedConfig {
    let loaded = load_lua(script).expect("load_lua should succeed");
    let resolved = resolve_source_with_format(
        Path::new("oxiroute.lua"),
        script.as_bytes(),
        ConfigFormat::Lua,
    )
    .expect("resolve_source_with_format should succeed");
    assert_eq!(resolved.config, loaded);
    loaded
}

#[test]
fn config_defaults_and_oxiroute_alias() {
    let via_oxi = eval_both("return oxi.config({})");
    let via_alias = eval_both("return oxiroute.config({})");
    assert_eq!(via_oxi, via_alias);
    assert_eq!(via_oxi.as_draft().version, 1);
    assert!(via_oxi.as_draft().listeners.is_empty());

    let with_opts = eval_both(
        r"
        return oxi.config({
            max_connections = 4096,
            http_fast_lane = false,
        })
        ",
    );
    assert_eq!(with_opts.as_draft().max_connections, Some(4096));
    assert_eq!(with_opts.as_draft().http_fast_lane, Some(false));
}

#[test]
fn certificate_builders_and_tls_profile() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            certificates = {
                oxi.certbot("main", { "*.example.com", "example.com" }),
                oxi.certbot("wildcard_only", "*.wild.test", { root = "/srv/letsencrypt/" }),
                oxi.certbot("custom_dirs", "custom.test", {
                    live_directory_path = "/var/tls/custom/live",
                    archive_directory_path = "/var/tls/custom/archive",
                }),
                oxi.cert_files("manual", "manual.test", "/etc/tls/manual.pem", "/etc/tls/manual.key"),
                oxi.cert_self_signed("dev", { "dev.local", "localhost" }, {
                    validity_days = 14,
                    key_type = "rsa_2048",
                }),
            },
            tls_profiles = {
                oxi.tls_profile("single", "main"),
                oxi.tls_profile("multi", { "manual", "dev" }, {
                    default_certificate = "dev",
                    min_version = "1.3",
                    alpn = { "h2", "http/1.1" },
                }),
            },
        })
        "#,
    );

    let draft = cfg.as_draft();
    assert_eq!(draft.certificates.len(), 5);

    assert_eq!(draft.certificates[0].name, "main");
    assert_eq!(
        draft.certificates[0].dns_names,
        vec!["*.example.com", "example.com"]
    );
    assert_eq!(
        draft.certificates[0].source,
        CertificateSource::Certbot {
            live_directory_path: PathBuf::from("/etc/letsencrypt/live/example.com"),
            archive_directory_path: PathBuf::from("/etc/letsencrypt/archive/example.com"),
        }
    );

    assert_eq!(draft.certificates[1].name, "wildcard_only");
    assert_eq!(
        draft.certificates[1].source,
        CertificateSource::Certbot {
            live_directory_path: PathBuf::from("/srv/letsencrypt/live/wild.test"),
            archive_directory_path: PathBuf::from("/srv/letsencrypt/archive/wild.test"),
        }
    );

    assert_eq!(draft.certificates[2].name, "custom_dirs");
    assert_eq!(
        draft.certificates[2].source,
        CertificateSource::Certbot {
            live_directory_path: PathBuf::from("/var/tls/custom/live"),
            archive_directory_path: PathBuf::from("/var/tls/custom/archive"),
        }
    );

    assert_eq!(draft.certificates[3].name, "manual");
    assert_eq!(
        draft.certificates[3].source,
        CertificateSource::Files {
            certificate_chain_path: PathBuf::from("/etc/tls/manual.pem"),
            private_key_path: PathBuf::from("/etc/tls/manual.key"),
        }
    );

    assert_eq!(draft.certificates[4].name, "dev");
    assert_eq!(
        draft.certificates[4].source,
        CertificateSource::SelfSignedDevelopment {
            validity_days: 14,
            key_type: SelfSignedKeyType::Rsa2048,
        }
    );

    assert_eq!(draft.tls_profiles.len(), 2);
    assert_eq!(draft.tls_profiles[0].default_certificate, "main");
    assert_eq!(draft.tls_profiles[0].certificates, vec!["main"]);

    assert_eq!(draft.tls_profiles[1].default_certificate, "dev");
    assert_eq!(draft.tls_profiles[1].min_version, TlsVersion::Tls13);
    assert_eq!(
        draft.tls_profiles[1].alpn,
        vec![AlpnProtocol::H2, AlpnProtocol::Http11]
    );
}

#[test]
fn upstream_builder_supports_socket_dns_unix_and_tables() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            upstream_pools = {
                oxi.upstream("single_socket", "127.0.0.1:8080"),
                oxi.upstream("mixed", {
                    "127.0.0.1:8081",
                    "backend.internal:9000",
                    { endpoint = "127.0.0.1:8082", max_connections = 64 },
                    { name = "named", endpoint = { type = "dns", host = "api.internal", port = 8443 } },
                }, {
                    algorithm = "least_connections",
                    connect_timeout_ms = 2500,
                }),
                oxi.upstream("unix_pool", {
                    "unix:/run/oxiroute/app.sock",
                }),
                oxi.upstream("single_table", {
                    endpoint = "/run/oxiroute/worker.sock",
                }),
            },
        })
        "#,
    );

    let pools = &cfg.as_draft().upstream_pools;
    assert_eq!(pools.len(), 4);

    assert_eq!(pools[0].servers.len(), 1);
    assert_eq!(pools[0].servers[0].name, "s1");
    assert_eq!(
        pools[0].servers[0].endpoint,
        UpstreamEndpoint::Socket {
            address: "127.0.0.1:8080".parse().unwrap(),
        }
    );

    assert_eq!(pools[1].algorithm, UpstreamAlgorithm::LeastConnections);
    assert_eq!(pools[1].connect_timeout_ms, Some(2500));
    assert_eq!(pools[1].servers.len(), 4);
    assert_eq!(pools[1].servers[0].name, "s1");
    assert_eq!(pools[1].servers[1].name, "s2");
    assert_eq!(
        pools[1].servers[1].endpoint,
        UpstreamEndpoint::Dns {
            host: "backend.internal".to_owned(),
            port: 9000,
        }
    );
    assert_eq!(pools[1].servers[2].name, "s3");
    assert_eq!(pools[1].servers[2].max_connections, Some(64));
    assert_eq!(pools[1].servers[3].name, "named");

    assert_eq!(
        pools[2].servers[0].endpoint,
        UpstreamEndpoint::Unix {
            path: PathBuf::from("/run/oxiroute/app.sock"),
        }
    );
    assert_eq!(pools[3].servers[0].name, "s1");
    assert_eq!(
        pools[3].servers[0].endpoint,
        UpstreamEndpoint::Unix {
            path: PathBuf::from("/run/oxiroute/worker.sock"),
        }
    );
}

#[test]
fn host_path_and_fixed_response_routes() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            http_services = {
                oxi.http_service("web", {
                    oxi.route("=/health", oxi.fixed_response(200, "ok\n", {
                        ["x-z"] = "last",
                        ["content-type"] = "text/plain",
                    }), {
                        methods = { "GET", "HEAD" },
                    }),
                    oxi.route("^/raw", oxi.fixed_response(204, nil, {
                        { name = "x-custom", value = "1" },
                    }), {
                        host = "*.example.com",
                    }),
                }, {
                    upstream_io_timeout_ms = 15000,
                }),
            },
        })
        "#,
    );

    let service = &cfg.as_draft().http_services[0];
    assert_eq!(service.upstream_io_timeout_ms, 15_000);
    assert_eq!(service.routes.len(), 2);

    assert_eq!(
        service.routes[0].path,
        HttpPathSelector::Exact {
            value: "/health".to_owned(),
        }
    );
    assert_eq!(service.routes[0].methods, vec!["GET", "HEAD"]);
    let HttpRouteAction::FixedResponse {
        status,
        body,
        headers,
    } = &service.routes[0].action
    else {
        panic!("expected fixed_response");
    };
    assert_eq!(*status, 200);
    assert_eq!(body, "ok\n");
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0].name, "content-type");
    assert_eq!(headers[0].value, "text/plain");
    assert_eq!(headers[1].name, "x-z");
    assert_eq!(headers[1].value, "last");

    assert_eq!(
        service.routes[1].path,
        HttpPathSelector::RawPrefix {
            value: "/raw".to_owned(),
        }
    );
    assert_eq!(
        service.routes[1].host,
        Some(HttpHostSelector::NginxLeadingWildcard {
            value: "example.com".to_owned(),
        })
    );
}

#[test]
fn redirect_routes_literal_and_template() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            http_services = {
                oxi.http_service("redirects", {
                    oxi.route("/legacy", oxi.redirect("https://example.com/v2", {
                        status = 301,
                        headers = { ["cache-control"] = "public, max-age=3600" },
                    }), {
                        host = ".example.com",
                    }),
                    oxi.route("/https-only", oxi.redirect("https://$host$request_uri", {
                        nginx_host_fallback = "example.com",
                    }), {
                        host = oxi.host({ kind = "exact_authority", value = "example.com:80" }),
                    }),
                }),
            },
        })
        "#,
    );

    let routes = &cfg.as_draft().http_services[0].routes;
    assert_eq!(routes.len(), 2);

    assert_eq!(
        routes[0].host,
        Some(HttpHostSelector::NginxLeadingDot {
            value: "example.com".to_owned(),
        })
    );
    let HttpRouteAction::Redirect {
        status,
        location,
        headers,
    } = &routes[0].action
    else {
        panic!("expected redirect");
    };
    assert_eq!(*status, 301);
    assert_eq!(
        *location,
        HttpRedirectLocation::Literal {
            value: "https://example.com/v2".to_owned(),
        }
    );
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].name, "cache-control");

    assert_eq!(
        routes[1].host,
        Some(HttpHostSelector::ExactAuthority {
            value: "example.com:80".to_owned(),
        })
    );
    let HttpRouteAction::Redirect {
        status, location, ..
    } = &routes[1].action
    else {
        panic!("expected redirect");
    };
    assert_eq!(*status, 308);
    assert_eq!(
        *location,
        HttpRedirectLocation::RequestTemplate {
            value: "https://$host$request_uri".to_owned(),
            nginx_host_fallback: Some("example.com".to_owned()),
        }
    );
}

#[test]
fn static_files_and_proxy_routes() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            upstream_pools = {
                oxi.upstream("api", "127.0.0.1:3000"),
            },
            http_services = {
                oxi.http_service("web", {
                    oxi.route("/assets", oxi.static_files("/var/www/assets", {
                        autoindex = false,
                        headers = { ["cache-control"] = "immutable" },
                    }), {
                        host = "cdn.example.com",
                    }),
                    oxi.route(oxi.path("/api"), oxi.proxy("api", {
                        queue_headers = true,
                    }), {
                        host = "api.example.com",
                        methods = "POST",
                    }),
                }),
            },
        })
        "#,
    );

    let routes = &cfg.as_draft().http_services[0].routes;
    assert_eq!(routes.len(), 2);

    assert_eq!(
        routes[0].host,
        Some(HttpHostSelector::NormalizedHost {
            value: "cdn.example.com".to_owned(),
        })
    );
    let HttpRouteAction::StaticFiles {
        root_directory,
        headers,
        ..
    } = &routes[0].action
    else {
        panic!("expected static_files");
    };
    assert_eq!(root_directory, Path::new("/var/www/assets"));
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].name, "cache-control");

    assert_eq!(
        routes[1].path,
        HttpPathSelector::SegmentPrefix {
            value: "/api".to_owned(),
        }
    );
    assert_eq!(routes[1].methods, vec!["POST"]);
    let HttpRouteAction::Proxy {
        upstream_pool,
        policy,
    } = &routes[1].action
    else {
        panic!("expected proxy");
    };
    assert_eq!(upstream_pool, "api");
    assert!(policy.queue_headers);
}

#[test]
fn listener_builder_supports_socket_unix_and_opts() {
    let cfg = eval_both(
        r#"
        return oxi.config({
            http_services = {
                oxi.http_service("web", {
                    oxi.route("/", oxi.fixed_response(200, "ok")),
                }),
            },
            listeners = {
                oxi.listener("http", "0.0.0.0:8080", "web", { max_connections = 1024 }),
                oxi.listener("unix_http", "/run/oxiroute/http.sock", "web"),
            },
        })
        "#,
    );

    let listeners = &cfg.as_draft().listeners;
    assert_eq!(listeners.len(), 2);
    assert_eq!(listeners[0].protocol, Protocol::Http);
    assert_eq!(listeners[0].max_connections, Some(1024));
    assert_eq!(
        listeners[0].bind,
        ListenerBind::Socket {
            address: "0.0.0.0:8080".parse().unwrap(),
            ipv6_only: false,
        }
    );
    assert_eq!(
        listeners[1].bind,
        ListenerBind::Unix {
            path: PathBuf::from("/run/oxiroute/http.sock"),
            mode: None,
        }
    );
}

#[test]
fn realistic_compact_multi_domain_production_config() {
    let script = r#"
        return oxi.config({
            certificates = {
                oxi.certbot("site", { "example.com", "www.example.com", "api.example.com" }),
                oxi.certbot("blog", "blog.example.org"),
            },
            tls_profiles = {
                oxi.tls_profile("edge", { "site", "blog" }, { min_version = "1.3" }),
            },
            upstream_pools = {
                oxi.upstream("app", { "127.0.0.1:8080", "127.0.0.1:8081" }),
                oxi.upstream("blog_backend", "127.0.0.1:9000"),
            },
            http_services = {
                oxi.http_service("http_redirect", {
                    oxi.route("/", oxi.redirect("https://$host$request_uri")),
                }),
                oxi.http_service("edge_web", {
                    oxi.route("=/healthz", oxi.fixed_response(200, "ok", { ["content-type"] = "text/plain" })),
                    oxi.route("/static", oxi.static_files("/var/www/site"), { host = "example.com" }),
                    oxi.route("/", oxi.redirect("https://example.com$request_uri"), { host = "www.example.com" }),
                    oxi.route("/api", oxi.proxy("app"), { host = "api.example.com" }),
                    oxi.route("/", oxi.proxy("blog_backend"), { host = "blog.example.org" }),
                }),
            },
            listeners = {
                oxi.listener("http", "0.0.0.0:80", "http_redirect"),
                oxi.listener("https", "0.0.0.0:443", "edge_web", { tls_profile = "edge" }),
            },
        })
    "#;

    let cfg = eval_both(script);
    let draft = cfg.as_draft();

    assert_eq!(draft.version, 1);
    assert_eq!(draft.certificates.len(), 2);
    assert_eq!(draft.tls_profiles.len(), 1);
    assert_eq!(draft.tls_profiles[0].default_certificate, "site");
    assert_eq!(draft.tls_profiles[0].min_version, TlsVersion::Tls13);
    assert_eq!(draft.upstream_pools.len(), 2);
    assert_eq!(draft.upstream_pools[0].servers.len(), 2);
    assert_eq!(draft.http_services.len(), 2);
    assert_eq!(draft.http_services[1].routes.len(), 5);
    assert_eq!(draft.listeners.len(), 2);
    assert_eq!(draft.listeners[1].tls_profile.as_deref(), Some("edge"));
}
