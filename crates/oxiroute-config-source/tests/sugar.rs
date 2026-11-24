use std::path::Path;

use oxiroute_config::{
    AlpnProtocol, CertificateSource, HttpHostSelector, HttpPathSelector, HttpRedirectLocation,
    HttpRouteAction, ListenerBind, StatsPageAdminPolicy, TlsVersion, UpstreamAlgorithm,
    UpstreamEndpoint,
};
use oxiroute_config_source::{ConfigFormat, ConfigSourceError, resolve_source_with_format};

const CROSS_FORMAT_YAML: &[u8] = br#"
edge:
  http: "0.0.0.0:80"
  https: "[::]:443"
  redirect_https: true
  min_version: "1.3"
  alpn: ["h2", "http/1.1"]
  max_connections: 4096
  stats: "127.0.0.1:8404/stats"
sites:
  app:
    host: "app.example.com"
    cert: "self_signed"
    proxy: "127.0.0.1:3000"
    gzip: true
    routes:
      - path: "=/health"
        respond:
          status: 200
          body: "ok"
"#;

const CROSS_FORMAT_TOML: &[u8] = br#"
[edge]
http = "0.0.0.0:80"
https = "[::]:443"
redirect_https = true
min_version = "1.3"
alpn = ["h2", "http/1.1"]
max_connections = 4096
stats = "127.0.0.1:8404/stats"

[sites.app]
host = "app.example.com"
cert = "self_signed"
proxy = "127.0.0.1:3000"
gzip = true

[[sites.app.routes]]
path = "=/health"
respond = { status = 200, body = "ok" }
"#;

const CROSS_FORMAT_KDL: &[u8] = br#"
(object)edge {
  http "0.0.0.0:80"
  https "[::]:443"
  redirect_https #true
  min_version "1.3"
  (array)alpn {
    - "h2"
    - "http/1.1"
  }
  max_connections 4096
  stats "127.0.0.1:8404/stats"
}
(object)sites {
  (object)app {
    host "app.example.com"
    cert "self_signed"
    proxy "127.0.0.1:3000"
    gzip #true
    (array)routes {
      (object)- {
        path "=/health"
        (object)respond {
          status 200
          body "ok"
        }
      }
    }
  }
}
"#;

const CROSS_FORMAT_LUA: &[u8] = br#"
return {
  edge = {
    http = "0.0.0.0:80",
    https = "[::]:443",
    redirect_https = true,
    min_version = "1.3",
    alpn = { "h2", "http/1.1" },
    max_connections = 4096,
    stats = "127.0.0.1:8404/stats",
  },
  sites = {
    app = {
      host = "app.example.com",
      cert = "self_signed",
      proxy = "127.0.0.1:3000",
      gzip = true,
      routes = {
        {
          path = "=/health",
          respond = { status = 200, body = "ok" },
        },
      },
    },
  },
}
"#;

#[test]
fn resolves_identical_sugar_config_across_yaml_toml_kdl_and_lua() {
    let yaml_res =
        resolve_source_with_format(Path::new("cfg.yaml"), CROSS_FORMAT_YAML, ConfigFormat::Yaml)
            .expect("YAML sugar resolves");
    let toml_res =
        resolve_source_with_format(Path::new("cfg.toml"), CROSS_FORMAT_TOML, ConfigFormat::Toml)
            .expect("TOML sugar resolves");
    let kdl_res =
        resolve_source_with_format(Path::new("cfg.kdl"), CROSS_FORMAT_KDL, ConfigFormat::Kdl)
            .expect("KDL sugar resolves");
    let lua_res =
        resolve_source_with_format(Path::new("cfg.lua"), CROSS_FORMAT_LUA, ConfigFormat::Lua)
            .expect("Lua sugar resolves");

    assert!(yaml_res.compositional);
    assert!(toml_res.compositional);
    assert!(kdl_res.compositional);
    assert!(lua_res.compositional);

    assert_eq!(yaml_res.config, toml_res.config);
    assert_eq!(yaml_res.config, kdl_res.config);
    assert_eq!(yaml_res.config, lua_res.config);
    assert_eq!(yaml_res.canonical_kdl, toml_res.canonical_kdl);
    assert_eq!(yaml_res.canonical_kdl, kdl_res.canonical_kdl);
    assert_eq!(yaml_res.canonical_kdl, lua_res.canonical_kdl);

    let cfg = yaml_res.config.as_draft();
    assert_eq!(cfg.version, 1);
    assert_eq!(cfg.certificates.len(), 1);
    assert_eq!(cfg.certificates[0].name, "app");
    assert_eq!(cfg.tls_profiles.len(), 1);
    assert_eq!(cfg.tls_profiles[0].name, "edge-tls");
    assert_eq!(cfg.tls_profiles[0].min_version, TlsVersion::Tls13);
    assert_eq!(
        cfg.tls_profiles[0].alpn,
        vec![AlpnProtocol::H2, AlpnProtocol::Http11]
    );
    assert_eq!(cfg.tls_profiles[0].default_certificate, "app");

    // Dual-stack [::]:443 listener has ipv6_only == false.
    let https_listener = cfg
        .listeners
        .iter()
        .find(|listener| listener.name == "edge-https")
        .expect("edge-https listener");
    assert_eq!(https_listener.max_connections, Some(4096));
    assert_eq!(https_listener.tls_profile.as_deref(), Some("edge-tls"));
    assert!(matches!(
        https_listener.bind,
        ListenerBind::Socket {
            ipv6_only: false,
            ..
        }
    ));

    let stats = cfg.stats.as_ref().expect("stats configured");
    assert_eq!(stats.pages.len(), 1);
    assert_eq!(stats.pages[0].uri_prefix, "/stats");
    assert_eq!(stats.pages[0].admin, StatsPageAdminPolicy::Disabled);
}

const MULTI_SITE_YAML: &[u8] = br#"
edge:
  http: ["0.0.0.0:80", "[::]:80"]
  https: "[::]:443"
  redirect_https: true
  default_cert: "brauliobo"
  stats:
    bind: "127.0.0.1:8404"
    uri_prefix: "/status"
    refresh_ms: 5000
    admin: "localhost"
  management: "127.0.0.1:9090"

sites:
  brauliobo:
    hosts: ["brauliobo.com", "*.brauliobo.com"]
    cert: "certbot"
    proxy:
      to: ["127.0.0.1:3001", "127.0.0.1:3002"]
      algorithm: "least_connections"
      health_check:
        type: "http"
        path: "/healthz"
      policy:
        queue_headers: true
    gzip:
      level: 6
      min_length_bytes: 256
    max_request_body_bytes: 2097152
    routes:
      - path: "=/health"
        methods: ["GET", "HEAD"]
        respond:
          status: 200
          body: "healthy"
          headers:
            X-Check: "ok"
      - path: "^/raw"
        proxy: "127.0.0.1:5000"
        headers:
          X-Backend: "raw"
      - path: "/api"
        proxy: "app.internal:8080"
        policy:
          read_timeout_ms: 15000

  api:
    host: "api.brauliobo.com"
    cert: "brauliobo"
    proxy: "/run/api.sock"

  docs:
    host: ".docs.example.com"
    cert:
      cert: "/etc/ssl/docs/fullchain.pem"
      key: "/etc/ssl/docs/privkey.pem"
    static:
      root: "/var/www/docs"
      spa: true
      autoindex: true
      headers:
        Cache-Control: "public, max-age=3600"

  old:
    host: "old.example.com"
    cert:
      type: "certbot"
      root: "/var/lib/certbot/"
    redirect:
      to: "https://brauliobo.com$request_uri"
      status: 301
      headers:
        X-Moved: "yes"

  acme:
    host: "acme.example.com"
    http_only: true
    static: "/var/www/acme"
"#;

#[test]
fn multi_site_certbot_wildcard_reuse_all_actions_routes_and_redirect() {
    let resolved =
        resolve_source_with_format(Path::new("edge.yaml"), MULTI_SITE_YAML, ConfigFormat::Yaml)
            .expect("multi-site edge config resolves");
    let cfg = resolved.config.as_draft();

    assert_multi_site_certs_and_pools(cfg);
    assert_multi_site_redirect_and_listeners(cfg);
    assert_multi_site_https_routes_and_telemetry(cfg);
}

fn assert_multi_site_certs_and_pools(cfg: &oxiroute_config::ConfigDraft) {
    // 3 generated certs (brauliobo, docs, old); `api` reuses `brauliobo`, `acme` is http_only.
    assert_eq!(cfg.certificates.len(), 3);
    let brauliobo_cert = cfg
        .certificates
        .iter()
        .find(|cert| cert.name == "brauliobo")
        .expect("brauliobo cert");
    assert_eq!(
        brauliobo_cert.dns_names,
        vec!["brauliobo.com", "*.brauliobo.com"]
    );
    assert!(matches!(
        &brauliobo_cert.source,
        CertificateSource::Certbot {
            live_directory_path,
            archive_directory_path,
        } if live_directory_path == Path::new("/etc/letsencrypt/live/brauliobo.com")
            && archive_directory_path == Path::new("/etc/letsencrypt/archive/brauliobo.com")
    ));

    let docs_cert = cfg
        .certificates
        .iter()
        .find(|cert| cert.name == "docs")
        .expect("docs cert");
    assert_eq!(
        docs_cert.dns_names,
        vec!["docs.example.com", "*.docs.example.com"]
    );
    assert!(matches!(
        &docs_cert.source,
        CertificateSource::Files {
            certificate_chain_path,
            private_key_path,
        } if certificate_chain_path == Path::new("/etc/ssl/docs/fullchain.pem")
            && private_key_path == Path::new("/etc/ssl/docs/privkey.pem")
    ));

    let old_cert = cfg
        .certificates
        .iter()
        .find(|cert| cert.name == "old")
        .expect("old cert");
    assert!(matches!(
        &old_cert.source,
        CertificateSource::Certbot {
            live_directory_path,
            archive_directory_path,
        } if live_directory_path == Path::new("/var/lib/certbot/live/old.example.com")
            && archive_directory_path == Path::new("/var/lib/certbot/archive/old.example.com")
    ));

    // TLS profile has unique certs in deterministic order with explicit default_cert.
    let tls_profile = &cfg.tls_profiles[0];
    assert_eq!(tls_profile.name, "edge-tls");
    assert_eq!(tls_profile.default_certificate, "brauliobo");
    assert_eq!(tls_profile.certificates, vec!["brauliobo", "docs", "old"]);

    // Upstream pools: site-api, site-brauliobo, site-brauliobo-route-2, site-brauliobo-route-3.
    let brauliobo_pool = cfg
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "site-brauliobo")
        .expect("site-brauliobo pool");
    assert_eq!(
        brauliobo_pool.algorithm,
        UpstreamAlgorithm::LeastConnections
    );
    assert!(brauliobo_pool.health_check.is_some());
    assert_eq!(brauliobo_pool.servers.len(), 2);
    assert_eq!(brauliobo_pool.servers[0].name, "s1");
    assert_eq!(brauliobo_pool.servers[1].name, "s2");

    let api_pool = cfg
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "site-api")
        .expect("site-api pool");
    assert!(matches!(
        &api_pool.servers[0].endpoint,
        UpstreamEndpoint::Unix { path } if path == Path::new("/run/api.sock")
    ));

    let api_route_pool = cfg
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "site-brauliobo-route-3")
        .expect("site-brauliobo-route-3 pool");
    assert!(matches!(
        &api_route_pool.servers[0].endpoint,
        UpstreamEndpoint::Dns { host, port } if host == "app.internal" && *port == 8080
    ));
}

fn assert_multi_site_redirect_and_listeners(cfg: &oxiroute_config::ConfigDraft) {
    // edge-http-redirect has http_only `acme` route BEFORE the catch-all 308 redirect!
    let redirect_svc = cfg
        .http_services
        .iter()
        .find(|svc| svc.name == "edge-http-redirect")
        .expect("edge-http-redirect service");
    assert_eq!(redirect_svc.routes.len(), 2);
    assert_eq!(
        redirect_svc.routes[0].host,
        Some(HttpHostSelector::NormalizedHost {
            value: "acme.example.com".to_owned()
        })
    );
    assert!(matches!(
        redirect_svc.routes[0].action,
        HttpRouteAction::StaticFiles { .. }
    ));
    assert_eq!(redirect_svc.routes[1].host, None);
    assert!(matches!(
        &redirect_svc.routes[1].action,
        HttpRouteAction::Redirect {
            status: 308,
            location: HttpRedirectLocation::RequestTemplate { value, .. },
            ..
        } if value == "https://$host$request_uri"
    ));

    // HTTP listeners: 0.0.0.0:80 and [::]:80 with ipv6_only: true on [::]:80!
    let http_v6 = cfg
        .listeners
        .iter()
        .find(|listener| listener.name == "edge-http-2")
        .expect("edge-http-2 listener");
    assert!(matches!(
        http_v6.bind,
        ListenerBind::Socket {
            ipv6_only: true,
            ..
        }
    ));
}

fn assert_multi_site_https_routes_and_telemetry(cfg: &oxiroute_config::ConfigDraft) {
    // edge-https has gzip level 6, wildcard host selectors, exact/raw/segment paths, static SPA, redirect.
    let https_svc = cfg
        .http_services
        .iter()
        .find(|svc| svc.name == "edge-https")
        .expect("edge-https service");
    let gzip = https_svc.gzip.as_ref().expect("gzip enabled");
    assert_eq!(gzip.level, 6);
    assert_eq!(gzip.min_length_bytes, 256);

    assert!(https_svc.routes.iter().any(|route| {
        route.host
            == Some(HttpHostSelector::NginxLeadingWildcard {
                value: "brauliobo.com".to_owned(),
            })
            && route.path
                == HttpPathSelector::Exact {
                    value: "/health".to_owned(),
                }
            && route.methods == vec!["GET", "HEAD"]
            && matches!(
                &route.action,
                HttpRouteAction::FixedResponse {
                    status: 200,
                    body,
                    ..
                } if body == "healthy"
            )
    }));

    assert!(https_svc.routes.iter().any(|route| {
        route.host
            == Some(HttpHostSelector::NormalizedHost {
                value: "brauliobo.com".to_owned(),
            })
            && route.path
                == HttpPathSelector::RawPrefix {
                    value: "/raw".to_owned(),
                }
            && matches!(&route.action, HttpRouteAction::Proxy { policy, .. } if policy.response_headers.len() == 1)
    }));

    assert!(https_svc.routes.iter().any(|route| {
        route.host
            == Some(HttpHostSelector::NginxLeadingDot {
                value: "docs.example.com".to_owned(),
            })
            && matches!(
                &route.action,
                HttpRouteAction::StaticFiles {
                    spa_fallback: Some(fallback),
                    autoindex: true,
                    headers,
                    ..
                } if fallback == Path::new("index.html") && headers.len() == 1
            )
    }));

    assert!(https_svc.routes.iter().any(|route| {
        route.host
            == Some(HttpHostSelector::NormalizedHost {
                value: "old.example.com".to_owned(),
            })
            && matches!(
                &route.action,
                HttpRouteAction::Redirect {
                    status: 301,
                    location: HttpRedirectLocation::RequestTemplate { value, .. },
                    headers,
                } if value == "https://brauliobo.com$request_uri" && headers.len() == 1
            )
    }));

    // Management and Stats populated.
    let mgmt = cfg.management.as_ref().expect("management");
    assert_eq!(mgmt.bind.to_string(), "127.0.0.1:9090");
    let stats = cfg.stats.as_ref().expect("stats");
    assert_eq!(stats.pages[0].uri_prefix, "/status");
    assert_eq!(stats.pages[0].refresh_ms, 5_000);
    assert_eq!(stats.pages[0].admin, StatsPageAdminPolicy::Localhost);
}

#[test]
fn coexists_with_explicit_pools_listeners_tls_profiles_and_templates() {
    let yaml = br#"
templates:
  protected_site:
    cert: "shared-cert"
    gzip: true
    access_policy:
      type: "bearer_token_file"
      token_file_path: "/etc/oxiroute/api.token"
    proxy_policy:
      queue_headers: true

certificates:
  - name: "shared-cert"
    dns_names: ["app.example.com", "admin.example.com"]
    source:
      type: "self_signed_development"

tls_profiles:
  - name: "custom-tls"
    certificates: ["shared-cert"]
    default_certificate: "shared-cert"
    min_version: "1.3"
    alpn: ["h2", "http/1.1"]

upstream_pools:
  - name: "existing-backend"
    servers:
      - name: "primary"
        endpoint:
          type: "socket"
          address: "127.0.0.1:7000"

http_services:
  - name: "internal-health"
    routes:
      - path:
          kind: "exact"
          value: "/ready"
        action:
          type: "fixed_response"
          status: 200
          body: "ready"

listeners:
  - name: "internal"
    bind:
      type: "socket"
      address: "127.0.0.1:8081"
    protocol: "http"
    service: "internal-health"

edge:
  https: "127.0.0.1:8443"
  tls_profile: "custom-tls"

sites:
  - name: "app"
    use: "protected_site"
    host: "app.example.com"
    proxy: "@existing-backend"
  - name: "admin"
    use: "protected_site"
    host: "admin.example.com"
    proxy:
      to: "@existing-backend"
      policy:
        upstream_host:
          type: "literal"
          value: "admin.internal"
"#;

    let resolved = resolve_source_with_format(Path::new("coexist.yaml"), yaml, ConfigFormat::Yaml)
        .expect("coexistence config resolves");
    let cfg = resolved.config.as_draft();

    // Explicit entries preserved without clobbering or duplication.
    assert_eq!(cfg.certificates.len(), 1);
    assert_eq!(cfg.tls_profiles.len(), 1);
    assert_eq!(cfg.tls_profiles[0].name, "custom-tls");
    // `@existing-backend` did not create any new upstream pools!
    assert_eq!(cfg.upstream_pools.len(), 1);
    assert_eq!(cfg.upstream_pools[0].name, "existing-backend");
    // Both explicit `internal` and generated `edge-https` listeners exist.
    assert_eq!(cfg.listeners.len(), 2);
    assert_eq!(cfg.listeners[0].name, "internal");
    assert_eq!(cfg.listeners[1].name, "edge-https");
    assert_eq!(cfg.listeners[1].tls_profile.as_deref(), Some("custom-tls"));
    // Both explicit `internal-health` and generated `edge-https` services exist.
    assert_eq!(cfg.http_services.len(), 2);
    let edge_https = cfg
        .http_services
        .iter()
        .find(|svc| svc.name == "edge-https")
        .expect("edge-https service");
    assert_eq!(edge_https.routes.len(), 2);
    assert!(edge_https.routes[0].access_policy.is_some());
    assert!(edge_https.routes[1].access_policy.is_some());
    assert!(matches!(
        &edge_https.routes[0].action,
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } if upstream_pool == "existing-backend" && policy.queue_headers
    ));
}

#[test]
fn expands_vars_and_parameterized_templates_inside_edge_and_sites() {
    let yaml = br#"
vars:
  domain: "example.org"
  backend_host: "127.0.0.1"
  https_bind: "[::]:443"

templates:
  app_site:
    params:
      port: 3000
    cert: "certbot"
    gzip: true
    proxy: "{{backend_host}}:{{port}}"

edge:
  http: "0.0.0.0:80"
  https: "{{https_bind}}"
  redirect_https: true

sites:
  web:
    use:
      name: "app_site"
      port: 3100
    host: "web.{{domain}}"
  api:
    use:
      name: "app_site"
      port: 3200
    host: "api.{{domain}}"
"#;

    let resolved = resolve_source_with_format(Path::new("vars.yaml"), yaml, ConfigFormat::Yaml)
        .expect("vars + parameterized templates + sugar resolves");
    let cfg = resolved.config.as_draft();
    assert!(resolved.compositional);

    assert_eq!(cfg.certificates.len(), 2);
    assert_eq!(cfg.certificates[0].name, "api");
    assert_eq!(cfg.certificates[0].dns_names, vec!["api.example.org"]);
    assert_eq!(cfg.certificates[1].name, "web");
    assert_eq!(cfg.certificates[1].dns_names, vec!["web.example.org"]);

    let api_pool = cfg
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "site-api")
        .expect("site-api pool");
    assert_eq!(api_pool.servers[0].endpoint.to_string(), "127.0.0.1:3200");

    let web_pool = cfg
        .upstream_pools
        .iter()
        .find(|pool| pool.name == "site-web")
        .expect("site-web pool");
    assert_eq!(web_pool.servers[0].endpoint.to_string(), "127.0.0.1:3100");
}

#[test]
fn http_only_edge_and_sites_only_without_edge_work_cleanly() {
    // 1. HTTP-only edge (no redirect_https, no https) serves all sites on edge-http, including catch-all.
    let http_only_yaml = br#"
edge:
  http: "127.0.0.1:8080"
  stats: "127.0.0.1:8404"
sites:
  default:
    proxy: "127.0.0.1:3000"
"#;
    let res =
        resolve_source_with_format(Path::new("http.yaml"), http_only_yaml, ConfigFormat::Yaml)
            .expect("http-only edge resolves");
    let cfg = res.config.as_draft();
    assert_eq!(cfg.listeners.len(), 1);
    assert_eq!(cfg.listeners[0].service.as_deref(), Some("edge-http"));
    assert_eq!(cfg.http_services.len(), 1);
    assert_eq!(cfg.http_services[0].name, "edge-http");
    assert_eq!(cfg.http_services[0].routes[0].host, None);

    // 2. `sites` without `edge` emits certificates, edge-tls, upstream_pools, and edge-https cleanly.
    let sites_only_yaml = br#"
sites:
  web:
    host: "web.example.com"
    cert: "self_signed"
    proxy: "127.0.0.1:4000"
"#;
    let res2 = resolve_source_with_format(
        Path::new("sites_only.yaml"),
        sites_only_yaml,
        ConfigFormat::Yaml,
    )
    .expect("sites-only config resolves");
    let cfg2 = res2.config.as_draft();
    assert!(cfg2.listeners.is_empty());
    assert_eq!(cfg2.certificates.len(), 1);
    assert_eq!(cfg2.tls_profiles.len(), 1);
    assert_eq!(cfg2.tls_profiles[0].name, "edge-tls");
    assert_eq!(cfg2.http_services.len(), 1);
    assert_eq!(cfg2.http_services[0].name, "edge-https");
}

#[test]
fn canonical_configs_without_sugar_pass_through_unchanged() {
    let canonical = br"
version: 1
listeners: []
";
    let res = resolve_source_with_format(Path::new("plain.yaml"), canonical, ConfigFormat::Yaml)
        .expect("canonical config resolves");
    assert!(!res.compositional);
    assert!(res.config.as_draft().listeners.is_empty());
}

const INVALID_SUGAR_CASES: &[(&str, &str, &str)] = &[
    (
        "unknown edge key",
        r#"
edge:
  http: "0.0.0.0:80"
  bogus_option: true
sites:
  a:
    host: "a.example.com"
    proxy: "127.0.0.1:3000"
"#,
        "unknown key `bogus_option`",
    ),
    (
        "unknown site key",
        r#"
sites:
  a:
    host: "a.example.com"
    proxy: "127.0.0.1:3000"
    typo_field: 123
"#,
        "unknown key `typo_field`",
    ),
    (
        "unknown route key",
        r#"
sites:
  a:
    host: "a.example.com"
    routes:
      - path: "/api"
        proxy: "127.0.0.1:3000"
        unknown_route_key: 1
"#,
        "unknown key `unknown_route_key`",
    ),
    (
        "site missing action and routes",
        r#"
sites:
  empty:
    host: "empty.example.com"
"#,
        "must specify at least one action",
    ),
    (
        "multiple root actions",
        r#"
sites:
  dup:
    host: "dup.example.com"
    proxy: "127.0.0.1:3000"
    static: "/var/www"
"#,
        "more than one root action",
    ),
    (
        "multiple catch-all sites",
        r#"
sites:
  a:
    proxy: "127.0.0.1:3001"
  b:
    proxy: "127.0.0.1:3002"
"#,
        "at most one catch-all site",
    ),
    (
        "https without certs",
        r#"
edge:
  https: "[::]:443"
sites:
  a:
    host: "a.example.com"
    proxy: "127.0.0.1:3000"
"#,
        "requires at least one certificate",
    ),
    (
        "invalid edge min_version",
        r#"
edge:
  https: "[::]:443"
  min_version: "1.1"
sites:
  a:
    host: "a.example.com"
    cert: "self_signed"
    proxy: "127.0.0.1:3000"
"#,
        "edge.min_version must be `1.2` or `1.3`",
    ),
    (
        "http_only with cert",
        r#"
sites:
  a:
    host: "a.example.com"
    http_only: true
    cert: "certbot"
    proxy: "127.0.0.1:3000"
"#,
        "`http_only: true` cannot be combined with `cert`",
    ),
    (
        "conflicting management",
        r#"
management:
  bind: "127.0.0.1:9090"
edge:
  management: "127.0.0.1:9091"
  http: "127.0.0.1:8080"
sites:
  a:
    host: "a.example.com"
    proxy: "127.0.0.1:3000"
"#,
        "both top-level `management` and `edge.management`",
    ),
    (
        "unknown key in proxy object",
        r#"
sites:
  a:
    host: "a.example.com"
    proxy:
      to: "127.0.0.1:3000"
      bad_key: 1
"#,
        "unknown key `bad_key`",
    ),
    (
        "pool options on @pool ref",
        r#"
upstream_pools:
  - name: "existing"
    servers:
      - name: "s1"
        endpoint:
          type: "socket"
          address: "127.0.0.1:3000"
sites:
  a:
    host: "a.example.com"
    proxy:
      to: "@existing"
      algorithm: "least_connections"
"#,
        "cannot set pool options when referencing existing pool",
    ),
];

#[test]
fn rejects_invalid_sugar_fields_with_typed_config_errors() {
    for (label, source, expected_fragment) in INVALID_SUGAR_CASES {
        let err = resolve_source_with_format(
            Path::new("invalid.yaml"),
            source.as_bytes(),
            ConfigFormat::Yaml,
        )
        .expect_err(&format!("case `{label}` should fail"));
        assert!(
            matches!(err, ConfigSourceError::TypedConfig(_)),
            "case `{label}` expected TypedConfig, got {err:?}"
        );
        assert!(
            err.to_string().contains(expected_fragment),
            "case `{label}` error `{err}` did not contain `{expected_fragment}`"
        );
    }
}
