//! The daemon configuration: one reverse HTTP/3 service that uses every route action and policy.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use oxiroute_config::{
    AccessLogPolicy, AlpnProtocol, CacheKeyComponent, CachePurgeAuthorization, CacheStore,
    CacheSurrogateTags, Certificate, CertificateSource, ConfigDraft, DownstreamTimeoutPolicy,
    HttpAccessPolicy, HttpCachePolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite,
    HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpProxyPathRewrite, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy,
    HttpSameSite, HttpService, HttpStaticMimePolicy, HttpStaticPathMapping, HttpUpstreamHost,
    HttpVersion, HttpVersionPolicy, Listener, ListenerBind, Management, Protocol, TlsProfile,
    TlsVersion, UpstreamAlgorithm, UpstreamPool, UpstreamServer, UpstreamTls, ValidatedConfig,
};
use tempfile::TempDir;

use crate::{fixture_support, support};

pub const BEARER_TOKEN: &str = "0123456789abcdef0123456789abcdef";
pub const API_KEY: &str = "abcdef0123456789abcdef0123456789";
pub const PURGE_TOKEN: &str = "fedcba9876543210fedcba9876543210";
pub const MANAGEMENT_TOKEN: &str =
    "2d9e0b7f5c4a3e1d8f6b0a9c7e5d3b1f2a4c6e8d0b9f7a5c3e1d9b7f5a3c1e8d";
/// `openssl passwd -apr1 -salt abcdefgh secret`, user `alice`.
const HTPASSWD: &str = "alice:$apr1$abcdefgh$h9FWgUz3n9YxylKLlR5SQ/\n";
pub const LISTENER_CONNECTIONS: u64 = 8;
pub const SERVICE_TIMEOUT: Duration = Duration::from_millis(1500);
pub const ROUTE_BODY_LIMIT: u64 = 8;
const SERVICE_BODY_LIMIT: u64 = 64 * 1024;
const FILE_TIME: u64 = 1_700_000_000;
pub const BIG_FILE_BYTES: usize = 300_000;
pub const OVERSIZED_FILE_BYTES: u64 = 64 * 1024 * 1024 + 1;

pub struct Layout {
    pub directory: TempDir,
    pub access_log: PathBuf,
}

pub struct Addresses {
    pub listener: SocketAddr,
    pub management: SocketAddr,
    pub origin: SocketAddr,
    pub refuser: SocketAddr,
}

impl Layout {
    pub fn create() -> Self {
        let directory = TempDir::new().expect("golden directory");
        let layout = Self {
            access_log: directory.path().join("access.jsonl"),
            directory,
        };
        layout.write_static_tree();
        layout.write_secret("bearer.token", &format!("{BEARER_TOKEN}\n"));
        layout.write_secret("api-key.token", &format!("{API_KEY}\n"));
        layout.write_secret("purge.token", &format!("{PURGE_TOKEN}\n"));
        layout.write_secret("htpasswd", HTPASSWD);
        layout
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn write_secret(&self, name: &str, contents: &str) {
        fixture_support::write_file_with_mode(
            self.directory.path(),
            name,
            contents.as_bytes(),
            0o600,
        );
    }

    fn write_static_tree(&self) {
        let pattern = |length: usize| {
            (0..length)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>()
        };
        let files: [(&str, Vec<u8>); 13] = [
            ("public/static/ok.txt", b"0123456789".to_vec()),
            ("public/static/empty.txt", Vec::new()),
            ("public/static/page.html", b"<h1>page</h1>\n".to_vec()),
            ("public/static/data.json", b"{\"a\":1}".to_vec()),
            ("public/static/dir/index.html", b"<h1>index</h1>\n".to_vec()),
            ("public/static/sub/inner.txt", b"inner".to_vec()),
            ("public/static/big.bin", pattern(BIG_FILE_BYTES)),
            ("public/noetag/ok.txt", b"noetag".to_vec()),
            ("public/listing/a.txt", b"a".to_vec()),
            ("public/spa/index.html", b"spa-fallback".to_vec()),
            ("public/errs/404.html", b"file-404\n".to_vec()),
            ("public/errs/403.html", b"file-403\n".to_vec()),
            ("public/errs/forbidden.txt", b"redirected-403\n".to_vec()),
        ];
        for (name, contents) in files {
            let path = self.path(name);
            fs::create_dir_all(path.parent().expect("static parent")).expect("static directory");
            fs::write(&path, contents).expect("static file");
        }
        for directory in [
            "public/static/emptydir",
            "public/listing/subdir",
            "public/errs/emptydir",
        ] {
            fs::create_dir_all(self.path(directory)).expect("static directory");
        }
        fs::File::create(self.path("public/static/oversized.bin"))
            .and_then(|file| file.set_len(OVERSIZED_FILE_BYTES))
            .expect("sparse oversized static file");
        fix_times(&self.path("public"));
    }
}

fn fix_times(path: &Path) {
    let time = UNIX_EPOCH + Duration::from_secs(FILE_TIME);
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("static directory listing") {
            fix_times(&entry.expect("static entry").path());
        }
    }
    fs::File::open(path)
        .and_then(|file| file.set_modified(time))
        .expect("fixed static mtime");
}

pub fn build(layout: &Layout, addresses: &Addresses) -> ValidatedConfig {
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let key_path = layout.path("proxy-a-key.pem");
    fs::copy(key.path(), &key_path).expect("private key copy");
    let mut config = support::empty_config();
    config
        .cache_stores
        .push(CacheStore::memory("memory", 1024 * 1024));
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key_path,
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.upstream_pools = vec![
        pool(
            "origin",
            UpstreamAlgorithm::First,
            &[("origin", addresses.origin)],
        ),
        pool(
            "failover",
            UpstreamAlgorithm::First,
            &[("dead", addresses.refuser), ("live", addresses.origin)],
        ),
        pool(
            "dead",
            UpstreamAlgorithm::First,
            &[("dead", addresses.refuser)],
        ),
    ];
    config.http_services.push(service(layout));
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: addresses.listener,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(LISTENER_CONNECTIONS),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    config.management = Some(Management {
        bind: addresses.management,
        ui_dir: None,
        token_file: None,
    });
    config.validate().expect("valid golden configuration")
}

fn pool(name: &str, algorithm: UpstreamAlgorithm, servers: &[(&str, SocketAddr)]) -> UpstreamPool {
    UpstreamPool {
        name: name.into(),
        servers: servers
            .iter()
            .map(|(name, address)| UpstreamServer {
                name: (*name).into(),
                endpoint: support::socket_endpoint(*address),
                max_connections: None,
                dns_resolution: oxiroute_config::DnsResolutionPolicy::default(),
            })
            .collect(),
        endpoints: Vec::new(),
        algorithm,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: Some(support::ORIGIN_SERVER_NAME.into()),
            verify: oxiroute_config::UpstreamTlsVerify::Full,
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

fn service(layout: &Layout) -> HttpService {
    let mut routes = fixed_routes();
    routes.extend(static_routes(layout));
    routes.extend(access_routes(layout));
    routes.extend(proxy_routes(layout));
    HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes,
        automatic_response_headers: true,
        upstream_io_timeout_ms: u64::try_from(SERVICE_TIMEOUT.as_millis())
            .expect("service timeout"),
        max_request_body_bytes: Some(SERVICE_BODY_LIMIT),
        gzip: None,
        access_log: Some(AccessLogPolicy::File {
            path: layout.access_log.clone(),
        }),
    }
}

fn route(path: HttpPathSelector, action: HttpRouteAction) -> HttpRoute {
    HttpRoute {
        host: None,
        path,
        methods: Vec::new(),
        access_policy: None,
        policy: HttpRoutePolicy {
            max_request_body_bytes: Some(SERVICE_BODY_LIMIT),
            request_buffering: true,
            ..HttpRoutePolicy::default()
        },
        action,
    }
}

fn exact(value: &str) -> HttpPathSelector {
    HttpPathSelector::Exact {
        value: value.into(),
    }
}

fn prefix(value: &str) -> HttpPathSelector {
    HttpPathSelector::SegmentPrefix {
        value: value.into(),
    }
}

fn literal(name: &str, value: &str) -> HttpLiteralHeader {
    HttpLiteralHeader {
        name: name.into(),
        value: value.into(),
        always: true,
    }
}

fn fixed(status: u16, body: &str, headers: &[(&str, &str)]) -> HttpRouteAction {
    HttpRouteAction::FixedResponse {
        status,
        body: body.into(),
        headers: headers
            .iter()
            .map(|(name, value)| literal(name, value))
            .collect(),
    }
}

fn template(status: u16, value: &str, headers: &[(&str, &str)]) -> HttpRouteAction {
    HttpRouteAction::Redirect {
        status,
        location: HttpRedirectLocation::RequestTemplate {
            value: value.into(),
            nginx_host_fallback: None,
        },
        headers: headers
            .iter()
            .map(|(name, value)| literal(name, value))
            .collect(),
    }
}

fn fixed_routes() -> Vec<HttpRoute> {
    let large = "x".repeat(60 * 1024);
    let mut routes: Vec<HttpRoute> = [
        (
            "/fixed/ok",
            fixed(
                200,
                "hello",
                &[("x-fixed", "one"), ("content-type", "text/plain")],
            ),
        ),
        ("/fixed/empty", fixed(200, "", &[])),
        ("/fixed/204", fixed(204, "", &[("x-fixed", "204")])),
        ("/fixed/205", fixed(205, "", &[])),
        ("/fixed/304", fixed(304, "", &[("etag", "\"v1\"")])),
        ("/fixed/201", fixed(201, "created", &[("location", "/new")])),
        ("/fixed/404", fixed(404, "gone", &[])),
        ("/fixed/503", fixed(503, "busy", &[("retry-after", "30")])),
        (
            "/fixed/duplicates",
            fixed(
                200,
                "dup",
                &[
                    ("set-cookie", "a=1"),
                    ("set-cookie", "b=2"),
                    ("x-dup", "1"),
                    ("x-dup", "2"),
                ],
            ),
        ),
        (
            "/fixed/utf8",
            fixed(
                200,
                "h\u{e9}llo \u{2713}",
                &[("content-type", "text/plain; charset=utf-8")],
            ),
        ),
        ("/fixed/large", fixed(200, &large, &[])),
        (
            "/fixed/oversized-header",
            fixed(
                200,
                "x",
                &[
                    ("x-big-1", &"a".repeat(6_000)),
                    ("x-big-2", &"a".repeat(6_000)),
                    ("x-big-3", &"a".repeat(6_000)),
                ],
            ),
        ),
        (
            "/redir/301",
            HttpRouteAction::Redirect {
                status: 301,
                location: HttpRedirectLocation::Literal {
                    value: "https://target.example/landing".into(),
                },
                headers: Vec::new(),
            },
        ),
        (
            "/redir/302",
            template(
                302,
                "$scheme://$host/moved$request_uri",
                &[("x-redirect", "yes")],
            ),
        ),
        (
            "/redir/308",
            template(308, "$scheme://$host$request_uri", &[]),
        ),
    ]
    .into_iter()
    .map(|(path, action)| route(exact(path), action))
    .collect();
    let mut hosted = route(exact("/hosted"), fixed(200, "hosted", &[]));
    hosted.host = Some(HttpHostSelector::NormalizedHost {
        value: "only.example".into(),
    });
    let mut post_only = route(exact("/post-only"), fixed(200, "post-ok", &[]));
    post_only.methods = vec!["POST".into()];
    routes.extend([hosted, post_only]);
    routes
}

fn static_action(root: PathBuf, tune: impl FnOnce(&mut HttpRouteAction)) -> HttpRouteAction {
    let mut action = HttpRouteAction::StaticFiles {
        root_directory: root,
        path_mapping: HttpStaticPathMapping::Root,
        index_files: vec!["index.html".into()],
        internal_index_redirects: true,
        directory_redirects: true,
        spa_fallback: None,
        try_files: Vec::new(),
        autoindex: false,
        autoindex_exact_size: true,
        autoindex_local_time: false,
        etag: true,
        mime: HttpStaticMimePolicy {
            default_type: Some("application/octet-stream".into()),
            types: [
                "txt:text/plain",
                "html:text/html; charset=utf-8",
                "json:application/json",
            ]
            .iter()
            .filter_map(|entry| entry.split_once(':'))
            .map(|(extension, content_type)| oxiroute_config::HttpMimeType {
                extension: extension.into(),
                content_type: content_type.into(),
            })
            .collect(),
        },
        headers: vec![literal("x-static", "yes")],
        error_responses: Vec::new(),
    };
    tune(&mut action);
    action
}

fn static_routes(layout: &Layout) -> Vec<HttpRoute> {
    let root = layout.path("public");
    let errors = |responses| {
        move |action: &mut HttpRouteAction| {
            if let HttpRouteAction::StaticFiles {
                error_responses, ..
            } = action
            {
                *error_responses = responses;
            }
        }
    };
    let custom_404 = oxiroute_config::HttpStaticErrorResponse {
        statuses: vec![404],
        file: None,
        body: Some("custom-not-found".into()),
        headers: vec![literal("x-error", "static")],
        internal_redirect: None,
    };
    let file_404 = oxiroute_config::HttpStaticErrorResponse {
        statuses: vec![404],
        file: Some(PathBuf::from("errs/404.html")),
        body: None,
        headers: Vec::new(),
        internal_redirect: None,
    };
    let redirect_403 = oxiroute_config::HttpStaticErrorResponse {
        statuses: vec![403],
        file: Some(PathBuf::from("errs/403.html")),
        body: None,
        headers: vec![literal("x-from", "403")],
        internal_redirect: Some("/errs/forbidden.txt".into()),
    };
    vec![
        route(
            prefix("/static"),
            static_action(root.clone(), errors(vec![custom_404])),
        ),
        route(
            prefix("/errs"),
            static_action(root.clone(), errors(vec![file_404, redirect_403])),
        ),
        route(
            prefix("/noetag"),
            static_action(root.clone(), |action| {
                if let HttpRouteAction::StaticFiles { etag, .. } = action {
                    *etag = false;
                }
            }),
        ),
        route(
            prefix("/listing"),
            static_action(root.clone(), |action| {
                if let HttpRouteAction::StaticFiles { autoindex, .. } = action {
                    *autoindex = true;
                }
            }),
        ),
        route(
            prefix("/spa"),
            static_action(root, {
                let fallback = PathBuf::from("spa/index.html");
                move |action| {
                    if let HttpRouteAction::StaticFiles { spa_fallback, .. } = action {
                        *spa_fallback = Some(fallback);
                    }
                }
            }),
        ),
    ]
}

fn access_routes(layout: &Layout) -> Vec<HttpRoute> {
    let guarded = |path: &str, action, policy| {
        let mut guarded = route(prefix(path), action);
        guarded.access_policy = Some(policy);
        guarded
    };
    let bearer =
        |file: &str, header: &str, realm: Option<&str>| HttpAccessPolicy::BearerTokenFile {
            token_file_path: layout.path(file),
            header_name: header.into(),
            realm: realm.map(Into::into),
        };
    vec![
        guarded(
            "/auth/bearer",
            fixed(200, "secret-bearer", &[]),
            bearer("bearer.token", "authorization", Some("golden")),
        ),
        guarded(
            "/auth/api-key",
            fixed(200, "secret-key", &[]),
            bearer("api-key.token", "x-api-key", None),
        ),
        guarded(
            "/auth/basic",
            fixed(200, "secret-basic", &[]),
            HttpAccessPolicy::BasicHtpasswdFile {
                htpasswd_file_path: layout.path("htpasswd"),
                realm: "golden-basic".into(),
            },
        ),
        guarded(
            "/auth/proxy",
            proxy("origin", HttpProxyPolicy::default()),
            bearer("bearer.token", "authorization", None),
        ),
    ]
}

fn proxy(pool: &str, policy: HttpProxyPolicy) -> HttpRouteAction {
    HttpRouteAction::Proxy {
        upstream_pool: pool.into(),
        policy: policy.into(),
    }
}

fn set(name: &str, value: HttpRequestHeaderValue) -> HttpRequestHeaderMutation {
    HttpRequestHeaderMutation::Set {
        name: name.into(),
        value,
    }
}

fn header_policy() -> HttpProxyPolicy {
    let literal_value = |value: &str| HttpRequestHeaderValue::Literal {
        value: value.into(),
    };
    HttpProxyPolicy {
        request_headers: vec![
            set("x-literal", literal_value("literal")),
            set("x-overridden", literal_value("policy")),
            set(
                "x-forwarded-for",
                HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            ),
            set("x-client-ip", HttpRequestHeaderValue::ClientIp),
            set("x-normalized-host", HttpRequestHeaderValue::NormalizedHost),
            set("x-authority", HttpRequestHeaderValue::IncomingAuthority),
            set("x-scheme", HttpRequestHeaderValue::DownstreamScheme),
            set(
                "x-joined",
                HttpRequestHeaderValue::IncomingHeader {
                    name: "x-source".into(),
                    max_bytes: 64,
                },
            ),
            set(
                "x-selected-host",
                HttpRequestHeaderValue::SelectedUpstreamHost,
            ),
            HttpRequestHeaderMutation::Remove {
                name: "x-remove-me".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-set".into(),
                value: "policy".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-add".into(),
                value: "policy".into(),
                always: false,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-add-always".into(),
                value: "policy".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-remove".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/internal".into(),
            to: "/".into(),
            mode: oxiroute_config::CookiePathMatch::Exact,
        }],
        response_cookie_attributes: vec![HttpCookieAttributePolicy {
            name: "sid".into(),
            secure: Some(false),
            http_only: Some(true),
            same_site: Some(HttpSameSite::Lax),
        }],
        ..HttpProxyPolicy::default()
    }
}

fn host_policy(upstream_host: HttpUpstreamHost) -> HttpProxyPolicy {
    HttpProxyPolicy {
        upstream_host,
        ..HttpProxyPolicy::default()
    }
}

fn retry_policy(max_retries: u8) -> HttpProxyPolicy {
    HttpProxyPolicy {
        retry: HttpRetryPolicy {
            max_retries,
            ..HttpRetryPolicy::default()
        },
        ..HttpProxyPolicy::default()
    }
}

fn cache_policy(purge_token: PathBuf) -> HttpProxyPolicy {
    HttpProxyPolicy {
        cache: Some(Box::new(HttpCachePolicy {
            store: "memory".into(),
            methods: vec!["GET".into(), "HEAD".into()],
            key_components: vec![
                CacheKeyComponent::Scheme,
                CacheKeyComponent::NormalizedHost,
                CacheKeyComponent::PathAndQuery,
            ],
            use_origin_cache_control: true,
            default_ttl_ms: 60_000,
            status_ttls: Vec::new(),
            grace_ms: 30_000,
            keep_ms: 300_000,
            revalidate: true,
            collapsed_forwarding: true,
            stale_on: Vec::new(),
            bypass_request: Vec::new(),
            no_store_request: Vec::new(),
            no_store_response: Vec::new(),
            set_cookie_policy: oxiroute_config::CacheSetCookiePolicy::default(),
            authorization_policy: oxiroute_config::CacheAuthorizationPolicy::default(),
            vary_policy: oxiroute_config::CacheVaryPolicy::default(),
            surrogate_tags: Some(CacheSurrogateTags {
                response_header: "surrogate-key".into(),
                max_tags: 16,
                max_tag_bytes: 64,
            }),
            purge_authorization: Some(CachePurgeAuthorization::BearerTokenFile {
                token_file_path: purge_token,
            }),
        })),
        ..HttpProxyPolicy::default()
    }
}

fn proxy_routes(layout: &Layout) -> Vec<HttpRoute> {
    let rewrite = HttpProxyPolicy {
        upstream_path_rewrite: Some(HttpProxyPathRewrite {
            from: "/rw".into(),
            to: "/rewritten".into(),
        }),
        ..HttpProxyPolicy::default()
    };
    let mut limited = route(
        prefix("/limit"),
        proxy("origin", HttpProxyPolicy::default()),
    );
    limited.policy.max_request_body_bytes = Some(ROUTE_BODY_LIMIT);
    vec![
        route(prefix("/p"), proxy("origin", HttpProxyPolicy::default())),
        route(prefix("/pol"), proxy("origin", header_policy())),
        route(prefix("/rw"), proxy("origin", rewrite)),
        route(
            prefix("/host-literal"),
            proxy(
                "origin",
                host_policy(HttpUpstreamHost::Literal {
                    value: "literal.example:9443".into(),
                }),
            ),
        ),
        route(
            prefix("/host-endpoint"),
            proxy(
                "origin",
                host_policy(HttpUpstreamHost::Endpoint {
                    unix_fallback: None,
                }),
            ),
        ),
        route(
            prefix("/host-nginx"),
            proxy(
                "origin",
                host_policy(HttpUpstreamHost::NginxHost {
                    fallback: "fallback.example".into(),
                }),
            ),
        ),
        route(prefix("/failover"), proxy("failover", retry_policy(2))),
        route(prefix("/no-retry"), proxy("failover", retry_policy(0))),
        route(prefix("/dead"), proxy("dead", retry_policy(1))),
        route(
            prefix("/retry-status"),
            proxy(
                "origin",
                HttpProxyPolicy {
                    retry: HttpRetryPolicy {
                        max_retries: 1,
                        response_statuses: vec![503],
                        target: oxiroute_config::HttpRetryTarget::SameServer,
                        ..HttpRetryPolicy::default()
                    },
                    ..HttpProxyPolicy::default()
                },
            ),
        ),
        limited,
        route(
            prefix("/c"),
            proxy("origin", cache_policy(layout.path("purge.token"))),
        ),
    ]
}
