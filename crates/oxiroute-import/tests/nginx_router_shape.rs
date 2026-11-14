#![cfg(unix)]

use std::{collections::BTreeMap, fs, path::Path};

use openssl::{
    asn1::Asn1Time,
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::PKey,
    x509::{X509, X509NameBuilder, extension::SubjectAlternativeName},
};
use oxiroute_config::{
    AccessLogPolicy, AlpnProtocol, CertificateSource, ConfigDraft, CookiePathMatch,
    HttpAccessPolicy, HttpHostSelector, HttpPathSelector, HttpProxyPolicy, HttpRoute,
    HttpRouteAction, ListenerBind, UpstreamTlsVerify,
};
use oxiroute_import::{
    Severity,
    nginx::{
        NginxBearerTokenOverlay, NginxDefaultAccessLogOverlay, NginxImportOptions,
        NginxImportReport, import_root_with_options,
    },
};
use tempfile::TempDir;

const LINEAGES: [(&str, &[&str]); 10] = [
    ("example.com", &["example.com", "*.example.com"]),
    ("example.org", &["example.org", "www.example.org"]),
    ("example.net", &["example.net"]),
    ("git.example.net", &["git.example.net"]),
    ("media.example.net", &["media.example.net"]),
    ("files.example.net", &["files.example.net"]),
    ("api.example.net", &["api.example.net"]),
    ("static.example.net", &["static.example.net"]),
    ("ws.example.net", &["ws.example.net"]),
    ("origin.example.net", &["origin.example.net"]),
];

fn write_lineage(live: &Path, lineage: &str, sans: &[&str]) {
    let key = PKey::from_ec_key(
        EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap(),
    )
    .unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", lineage).unwrap();
    let name = name.build();
    let mut builder = X509::builder().unwrap();
    builder.set_version(2).unwrap();
    builder
        .set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
        .unwrap();
    builder.set_subject_name(&name).unwrap();
    builder.set_issuer_name(&name).unwrap();
    builder.set_pubkey(&key).unwrap();
    builder
        .set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    builder
        .set_not_after(&Asn1Time::days_from_now(30).unwrap())
        .unwrap();
    let mut alternative_names = SubjectAlternativeName::new();
    for san in sans {
        alternative_names.dns(san);
    }
    let extension = alternative_names
        .build(&builder.x509v3_context(None, None))
        .unwrap();
    builder.append_extension(extension).unwrap();
    builder.sign(&key, MessageDigest::sha256()).unwrap();
    let directory = live.join(lineage);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("cert.pem"),
        builder.build().to_pem().unwrap(),
    )
    .unwrap();
}

fn copy_tree(from: &Path, to: &Path, live: &Path) -> usize {
    let mut files = 0;
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            files += copy_tree(&entry.path(), &target, live);
        } else {
            let source = fs::read_to_string(entry.path()).unwrap();
            fs::write(target, source.replace("@LIVE@", &live.to_string_lossy())).unwrap();
            files += 1;
        }
    }
    files
}

fn router_shape() -> (TempDir, usize) {
    let directory = tempfile::tempdir().unwrap();
    let live = directory.path().join("letsencrypt/live");
    for (lineage, sans) in LINEAGES {
        write_lineage(&live, lineage, sans);
    }
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nginx/synthetic/router-shape");
    let files = copy_tree(&source, &directory.path().join("etc"), &live);
    (directory, files)
}

fn options() -> NginxImportOptions {
    NginxImportOptions {
        x_accel_controls_absent: true,
        default_access_log: Some(NginxDefaultAccessLogOverlay {
            path: "/var/log/oxiroute/access.jsonl".into(),
        }),
        bearer_tokens: vec![NginxBearerTokenOverlay {
            server_name: "api.example.net".into(),
            token_file_path: "/run/secrets/api.token".into(),
        }],
        ..NginxImportOptions::default()
    }
}

fn import(directory: &TempDir) -> NginxImportReport {
    import_root_with_options(Path::new("etc/nginx.conf"), directory.path(), &options())
}

fn routes<'a>(config: &'a ConfigDraft, listener: &str) -> &'a [HttpRoute] {
    let service = config
        .listeners
        .iter()
        .find(|candidate| {
            candidate.bind
                == ListenerBind::Socket {
                    address: listener.parse().unwrap(),
                }
        })
        .and_then(|candidate| candidate.service.as_deref())
        .unwrap_or_else(|| panic!("no listener {listener}"));
    &config
        .http_services
        .iter()
        .find(|candidate| candidate.name == service)
        .unwrap()
        .routes
}

fn warning_counts(report: &NginxImportReport) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for diagnostic in report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Warning)
    {
        *counts.entry(diagnostic.code().as_str()).or_insert(0) += 1;
    }
    counts
}

fn route<'a>(routes: &'a [HttpRoute], host: &str, prefix: &str) -> &'a HttpRoute {
    let selector = |route: &HttpRoute| match &route.host {
        None => "*".to_owned(),
        Some(HttpHostSelector::NormalizedHost { value }) => value.clone(),
        Some(HttpHostSelector::NginxLeadingWildcard { value }) => format!("*.{value}"),
        Some(other) => panic!("unexpected host selector {other:?}"),
    };
    routes
        .iter()
        .find(|route| {
            selector(route) == host
                && route.path
                    == HttpPathSelector::RawPrefix {
                        value: prefix.into(),
                    }
        })
        .unwrap_or_else(|| panic!("no route {host}{prefix}"))
}

fn proxy_policy(route: &HttpRoute) -> &HttpProxyPolicy {
    match &route.action {
        HttpRouteAction::Proxy { policy, .. } => policy,
        other => panic!("proxy route expected, got {other:?}"),
    }
}

#[test]
fn router_shape_skeleton_finalizes_end_to_end() {
    let (directory, files) = router_shape();
    assert_eq!(files, 21);
    let report = import(&directory);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(report.blocked_http_services.is_empty());
    assert_eq!(report.source_graph.sources.len(), 21);
    let config = report.candidate.validated().expect("finalized").as_draft();

    // One dual-stack listener per shared socket; 37 servers fold into 2 services.
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.http_services.len(), 2);
    assert_eq!(
        config.upstream_pools.len(),
        5,
        "3 unix upstreams and 2 direct https origins"
    );
    assert_eq!(config.certificates.len(), 10);
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.tls_profiles[0].alpn, [AlpnProtocol::Http11]);
    let plain = routes(config, "[::]:80");
    let secure = routes(config, "[::]:443");
    assert_eq!((plain.len(), secure.len()), (12, 30));
    for (name, sans) in LINEAGES {
        let certificate = config.certificates.iter().find(|certificate| {
            matches!(&certificate.source, CertificateSource::Certbot { live_directory_path, .. }
                if live_directory_path.ends_with(name))
        });
        let mut expected = sans.iter().map(ToString::to_string).collect::<Vec<_>>();
        expected.sort();
        let mut actual = certificate.map(|certificate| certificate.dns_names.clone());
        actual.iter_mut().for_each(|names| names.sort());
        assert_eq!(actual, Some(expected), "{name}");
    }
    assert!(config.http_services.iter().all(|service| {
        service.access_log
            == Some(AccessLogPolicy::File {
                path: "/var/log/oxiroute/access.jsonl".into(),
            })
    }));

    // Plain socket: ACME webroot, certbot redirect pair without the unreachable 404, catch-all.
    let HttpRouteAction::StaticFiles { root_directory, .. } =
        &route(plain, "*.example.com", "/.well-known/acme-challenge/").action
    else {
        panic!("ACME webroot is static");
    };
    assert_eq!(root_directory, Path::new("/var/www/letsencrypt"));
    assert!(matches!(
        route(plain, "example.org", "/").action,
        HttpRouteAction::Redirect { status: 301, .. }
    ));
    assert!(matches!(
        route(plain, "*", "/").action,
        HttpRouteAction::Redirect { status: 302, .. }
    ));
    assert_eq!(
        plain
            .iter()
            .filter(|route| matches!(
                route.action,
                HttpRouteAction::FixedResponse { status: 404, .. }
            ))
            .count(),
        0
    );

    // TLS socket: static root, bearer gate, cookie rewrite, large upload, websocket, https origins.
    let HttpRouteAction::StaticFiles { root_directory, .. } =
        &route(secure, "static.example.net", "/").action
    else {
        panic!("static root server");
    };
    assert_eq!(root_directory, Path::new("/var/www/static"));
    assert_eq!(
        route(secure, "api.example.net", "/").access_policy,
        Some(HttpAccessPolicy::BearerTokenFile {
            token_file_path: "/run/secrets/api.token".into(),
            header_name: "authorization".into(),
            realm: None,
        })
    );
    let cookie =
        &proxy_policy(route(secure, "media.example.net", "/")).response_cookie_path_rewrites;
    assert_eq!(cookie.len(), 1);
    assert_eq!(cookie[0].mode, CookiePathMatch::NginxPrefix);
    assert_eq!(cookie[0].to, "/; SameSite=none; Secure");
    let files = route(secure, "files.example.net", "/");
    assert!(!files.policy.request_buffering);
    assert_eq!(
        files.policy.max_request_body_bytes,
        Some(16_400 * 1_024 * 1_024)
    );
    assert_eq!(files.policy.read_timeout_ms, 3_600_000);
    assert_eq!(
        route(secure, "ws.example.net", "/socket/")
            .policy
            .read_timeout_ms,
        3_600_000
    );
    assert!(secure.iter().all(|route| !route.policy.response_buffering));
    let unverified = config
        .upstream_pools
        .iter()
        .filter(|pool| {
            pool.tls.as_ref().is_some_and(|tls| {
                tls.verify == UpstreamTlsVerify::None && tls.server_name.is_none()
            })
        })
        .count();
    assert_eq!(unverified, 2);

    assert_eq!(
        warning_counts(&report),
        BTreeMap::from([
            ("W_ACCESS_LOG_MIGRATED", 1),
            ("W_DIRECTIVE_NO_EFFECT", 6),
            ("W_DUAL_STACK_LISTENER", 2),
            ("W_PROXY_BUFFERING_STREAMED", 4),
            ("W_UPSTREAM_TLS_UNVERIFIED", 2),
        ])
    );
}

#[test]
fn router_shape_requires_exactly_the_operator_overlays_and_the_audit_assertion() {
    let (directory, _) = router_shape();
    let import = |options: &NginxImportOptions| {
        import_root_with_options(Path::new("etc/nginx.conf"), directory.path(), options)
    };
    let bare = import(&NginxImportOptions::default());
    assert!(bare.has_errors());
    assert!(bare.candidate.validated().is_none());
    let messages = bare
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Error)
        .map(oxiroute_import::Diagnostic::message)
        .collect::<Vec<_>>();
    for expected in [
        "formatted access_log output is not equivalent",
        "bearer authorization rule requires an explicit bearer token file overlay",
        "proxy_ignore_headers must explicitly disable",
    ] {
        assert!(
            messages.iter().any(|message| message.contains(expected)),
            "{expected}: {messages:#?}"
        );
    }
    assert!(!import(&options()).has_errors());
}
