#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::{
    AccessLogPolicy, ConfigDraft, CookiePathMatch, HttpCookiePathRewrite, HttpHostSelector,
    HttpPathSelector, HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpRouteAction,
    HttpStaticTryFile, HttpUpstreamHost, RtmpRecorderSegmentNaming, RtmpRecorderTimeBasis,
    RtmpRecorderTimezone, UpstreamEndpoint, UpstreamTls, UpstreamTlsVerify,
};
use oxiroute_import::{
    DeploymentRequirementKind, OperationalOverlayKind, Severity,
    nginx::{
        NginxBearerTokenOverlay, NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay,
        NginxHostTimezoneOverlay, NginxImportOptions, NginxImportReport, NginxRecordingRootOverlay,
        NginxUpstreamTlsOverlay, RootOccurrenceDisposition, import_root, import_root_with_options,
    },
};

#[test]
fn audited_absence_of_x_accel_controls_allows_modern_nginx_proxy_defaults() {
    let directory = tempfile::tempdir().expect("audited nginx root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"events {} http { access_log off; proxy_buffering off; upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:8080 default_server; location / { proxy_pass http://app; } } }",
    )
    .expect("write audited nginx root");

    let blocked = import_root(Path::new("nginx.conf"), directory.path());
    assert!(blocked.has_errors());
    assert!(
        blocked
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );

    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            x_accel_controls_absent: true,
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("audited proxy candidate");
    assert!(config.http_services[0].routes[0].policy.request_buffering);
}

#[test]
fn complete_root_merges_http_and_rtmp_and_externalizes_process_concerns() {
    let directory = tempfile::tempdir().expect("complete nginx root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"user www-data media;\nworker_processes 2;\nworker_rlimit_nofile 4096;\nerror_log /var/log/nginx/error.log warn;\nload_module modules/ngx_rtmp_module.so;\nevents { worker_connections 1024; multi_accept on; }\nhttp { access_log off; server { listen 127.0.0.1:18080 default_server; location / { return 200 ok; } } }\nrtmp { access_log off; chunk_size 8192; server { listen 127.0.0.1:1935; application live { live on; record all; record_path /var/lib/recordings; record_unique on; record_interval 1h; record_suffix -%Y%m%d_%H%M%S.mp4; push rtmp://127.0.0.1:1936/$name; } } }\n",
    )
    .expect("write complete nginx root");

    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            host_timezones: vec![NginxHostTimezoneOverlay {
                timezone: "America/Argentina/Buenos_Aires".into(),
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(report.blocked_http_services.is_empty());
    assert!(report.blocked_rtmp_services.is_empty());
    let config = report
        .candidate
        .validated()
        .expect("merged config")
        .as_draft();
    assert_eq!(config.listeners.len(), 2);
    assert_eq!(config.http_services.len(), 1);
    assert_eq!(config.rtmp_services.len(), 1);
    let rtmp = &config.rtmp_services[0];
    assert_eq!(rtmp.outbound_chunk_size, 8_192);
    assert_eq!(rtmp.access_log, Some(AccessLogPolicy::Disabled));
    assert_eq!(rtmp.applications[0].push_targets[0].application, "$name");
    let recorder = &rtmp.applications[0].recorders[0];
    assert_eq!(recorder.rotation_interval_ms, Some(3_600_000));
    assert_eq!(
        recorder.timezone,
        RtmpRecorderTimezone::Iana("America/Argentina/Buenos_Aires".into())
    );
    assert_eq!(recorder.time_basis, RtmpRecorderTimeBasis::SegmentStart);
    assert_eq!(
        recorder.segment_naming,
        RtmpRecorderSegmentNaming::NginxCompatible
    );
    assert_host_timezone_overlay_consumed(&report);
    assert_eq!(report.candidate.deployment_requirements.len(), 8);
    assert_eq!(
        report.root_occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );
    assert_eq!(
        report
            .root_occurrence_ledger
            .iter()
            .map(|decision| decision.occurrence)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        report.root_occurrence_ledger.len()
    );
    assert!(
        report
            .root_occurrence_ledger
            .iter()
            .any(|decision| { matches!(decision.disposition, RootOccurrenceDisposition::Http) })
    );
    assert!(
        report
            .root_occurrence_ledger
            .iter()
            .any(|decision| { matches!(decision.disposition, RootOccurrenceDisposition::Rtmp) })
    );
    assert!(report.root_occurrence_ledger.iter().any(|decision| {
        matches!(
            decision.disposition,
            RootOccurrenceDisposition::Deployment(_)
        )
    }));
    let requirements = &report.candidate.deployment_requirements;
    assert!(
        requirements
            .iter()
            .any(|requirement| requirement.kind == DeploymentRequirementKind::ProcessUser)
    );
    assert!(
        requirements
            .iter()
            .any(|requirement| requirement.kind == DeploymentRequirementKind::ProcessGroup)
    );
    assert!(
        requirements
            .iter()
            .any(|requirement| requirement.kind == DeploymentRequirementKind::ModuleLoad)
    );
    assert!(
        report
            .candidate
            .provenance
            .iter()
            .any(|entry| entry.path == "/listeners/1")
    );
}

fn assert_host_timezone_overlay_consumed(report: &oxiroute_import::nginx::NginxImportReport) {
    assert!(report.candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == OperationalOverlayKind::HostTimezone && overlay.satisfied
    }));
}

#[test]
fn host_timezone_overlay_must_be_unique_and_consumed_by_recording_lowering() {
    let directory = tempfile::tempdir().expect("host timezone overlay directory");
    fs::write(
        directory.path().join("nginx.conf"),
        explicit_proxy_root("http://127.0.0.1:3000"),
    )
    .expect("write HTTP-only root");
    let unused = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            host_timezones: vec![NginxHostTimezoneOverlay {
                timezone: "America/Argentina/Buenos_Aires".into(),
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(
        unused
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(unused.candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == OperationalOverlayKind::HostTimezone && !overlay.satisfied
    }));

    fs::write(
        directory.path().join("nginx.conf"),
        b"events { worker_connections 16; } rtmp { server { listen 127.0.0.1:1935; application live { live on; record all; record_path /var/lib/recordings; } } }",
    )
    .expect("write recording root");
    let duplicate = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            host_timezones: vec![
                NginxHostTimezoneOverlay {
                    timezone: "America/Argentina/Buenos_Aires".into(),
                },
                NginxHostTimezoneOverlay {
                    timezone: "America/Recife".into(),
                },
            ],
            ..NginxImportOptions::default()
        },
    );
    assert!(
        duplicate
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(
        duplicate
            .candidate
            .operational_overlays
            .iter()
            .filter(|overlay| overlay.kind == OperationalOverlayKind::HostTimezone)
            .count(),
        2
    );
    assert!(
        duplicate
            .candidate
            .operational_overlays
            .iter()
            .all(|overlay| {
                overlay.kind != OperationalOverlayKind::HostTimezone || !overlay.satisfied
            })
    );
}

const UNVERIFIED_TLS: UpstreamTls = UpstreamTls {
    server_name: None,
    verify: UpstreamTlsVerify::None,
    ca_certificate_path: None,
};

#[test]
fn bare_https_origins_lower_unverified_with_a_warning_and_need_no_overlay() {
    let directory = tempfile::tempdir().expect("HTTPS nginx root directory");
    for origin in ["https://origin.example.test:8443", "https://192.0.2.44"] {
        fs::write(
            directory.path().join("nginx.conf"),
            explicit_proxy_root(origin),
        )
        .expect("write HTTPS root");
        let report = import_root(Path::new("nginx.conf"), directory.path());
        assert!(!report.has_errors(), "{origin}: {:?}", report.diagnostics);
        assert_eq!(
            finalized(&report).upstream_pools[0].tls,
            Some(UNVERIFIED_TLS),
            "{origin}"
        );
        assert_eq!(
            warning_codes(&report),
            ["W_UPSTREAM_TLS_UNVERIFIED"],
            "{origin}"
        );
        assert!(
            report
                .candidate
                .operational_overlays
                .iter()
                .all(|overlay| overlay.kind != OperationalOverlayKind::UpstreamTlsPolicy),
            "{origin}"
        );
    }
}

#[test]
fn explicit_upstream_tls_overlay_verifies_the_origin_and_requests_activation() {
    let directory = tempfile::tempdir().expect("HTTPS nginx root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        explicit_proxy_root("https://192.0.2.44"),
    )
    .expect("write IP HTTPS root");
    let strict = UpstreamTls {
        server_name: Some("verified-origin.example.test".into()),
        verify: UpstreamTlsVerify::Full,
        ca_certificate_path: Some("/path/to/ca.pem".into()),
    };
    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            upstream_tls: vec![NginxUpstreamTlsOverlay {
                authority: "192.0.2.44".into(),
                tls: strict.clone(),
                require_connectivity_activation: true,
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    assert_eq!(finalized(&report).upstream_pools[0].tls, Some(strict));
    assert!(warning_codes(&report).is_empty());
    assert!(report.root_occurrence_ledger.iter().any(|decision| {
        matches!(
            decision.disposition,
            RootOccurrenceDisposition::Activation(_)
        )
    }));
}

#[test]
fn duplicate_unresolved_and_misspelled_security_overlays_never_finalize() {
    let directory = tempfile::tempdir().expect("strict overlay root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        explicit_proxy_root("https://192.0.2.44"),
    )
    .unwrap();
    let duplicate = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            upstream_tls: vec![
                tls_overlay("192.0.2.44", "origin.example", false),
                tls_overlay("192.0.2.44", "other.example", false),
            ],
            ..NginxImportOptions::default()
        },
    );
    assert!(
        duplicate
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(
        duplicate
            .candidate
            .operational_overlays
            .iter()
            .any(|overlay| {
                overlay.kind == OperationalOverlayKind::UpstreamTlsPolicy
                    && !overlay.satisfied
                    && overlay
                        .values
                        .iter()
                        .any(|value| value == "authority=192.0.2.44")
            })
    );

    let misspelled = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            upstream_tls: vec![tls_overlay("192.0.2.4", "origin.example", false)],
            ..NginxImportOptions::default()
        },
    );
    assert!(
        misspelled
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(misspelled.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("matches no lowered proxy origin")
    }));

    let directory = tempfile::tempdir().expect("bearer overlay root directory");
    fs::write(directory.path().join("nginx.conf"), bearer_proxy_root()).unwrap();
    let duplicate_bearer = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            bearer_tokens: vec![
                bearer_overlay("api.example.org", "/run/secrets/api.token"),
                bearer_overlay("API.example.org", "/run/secrets/other.token"),
            ],
            ..NginxImportOptions::default()
        },
    );
    assert!(duplicate_bearer.has_errors());
    assert!(
        duplicate_bearer
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
}

#[test]
fn bearer_token_overlay_lowers_a_redacted_authorization_rule() {
    let directory = tempfile::tempdir().expect("bearer overlay root directory");
    fs::write(directory.path().join("nginx.conf"), bearer_proxy_root()).unwrap();
    let missing = import_root(Path::new("nginx.conf"), directory.path());
    assert!(missing.has_errors());
    assert!(
        missing
            .candidate
            .operational_overlays
            .iter()
            .any(
                |overlay| overlay.kind == OperationalOverlayKind::BearerTokenFile
                    && !overlay.satisfied
            )
    );

    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            bearer_tokens: vec![bearer_overlay("api.example.org", "/run/secrets/api.token")],
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(report.candidate.validated().is_some());
    assert!(report.candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == OperationalOverlayKind::BearerTokenFile && overlay.satisfied
    }));
}

#[test]
fn bounded_downstream_scheme_specializes_static_authority_per_listener() {
    let directory = tempfile::tempdir().expect("scheme nginx root directory");
    let source = explicit_proxy_root("$scheme://192.0.2.45").replace(
        "listen 127.0.0.1:18080 default_server;",
        "listen 127.0.0.1:18080 default_server; listen 127.0.0.1:18443 ssl; ssl_certificate /missing/fullchain.pem; ssl_certificate_key /missing/privkey.pem; ssl_protocols TLSv1.2 TLSv1.3;",
    );
    fs::write(directory.path().join("nginx.conf"), source).expect("write scheme root");

    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            upstream_tls: vec![NginxUpstreamTlsOverlay {
                authority: "192.0.2.45".into(),
                tls: UpstreamTls {
                    server_name: Some("scheme-origin.example.test".into()),
                    verify: oxiroute_config::UpstreamTlsVerify::Full,
                    ca_certificate_path: None,
                },
                require_connectivity_activation: false,
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:?}", report.diagnostics);
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("scheme config");
    let plain = config
        .upstream_pools
        .iter()
        .find(|pool| pool.tls.is_none())
        .expect("HTTP origin pool");
    let secure = config
        .upstream_pools
        .iter()
        .find(|pool| pool.tls.is_some())
        .expect("HTTPS origin pool");
    assert!(matches!(
        plain.servers[0].endpoint,
        UpstreamEndpoint::Socket { address } if address.port() == 80
    ));
    assert!(matches!(
        secure.servers[0].endpoint,
        UpstreamEndpoint::Socket { address } if address.port() == 443
    ));
}

#[test]
fn included_http_and_rtmp_sources_load_as_one_stable_graph() {
    let report = import_root(Path::new("nginx.conf"), &synthetic_root());
    assert!(report.source_graph.snapshot_stable);
    assert_eq!(
        report.source_graph.sources.len(),
        3,
        "{:#?}",
        report.diagnostics
    );
}

#[test]
fn migration_overlays_finalize_the_synthetic_root_and_are_consumed() {
    let options = NginxImportOptions {
        host_timezones: vec![NginxHostTimezoneOverlay {
            timezone: "America/Argentina/Buenos_Aires".into(),
        }],
        default_access_log: Some(NginxDefaultAccessLogOverlay {
            path: "/var/lib/oxiroute/http-access.jsonl".into(),
        }),
        recording_root: Some(NginxRecordingRootOverlay {
            path: "/mnt/storage/recordings".into(),
        }),
        default_error_page: Some(NginxDefaultErrorPageOverlay {
            server: "nginx/1.30.2".into(),
        }),
        ..NginxImportOptions::default()
    };
    let report = import_root_with_options(Path::new("nginx.conf"), &synthetic_root(), &options);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    for kind in [
        OperationalOverlayKind::StructuredAccessLogMigration,
        OperationalOverlayKind::RecordingRootMigration,
        OperationalOverlayKind::DefaultErrorPageMigration,
    ] {
        assert!(
            report
                .candidate
                .operational_overlays
                .iter()
                .any(|overlay| overlay.kind == kind && overlay.satisfied),
            "{kind:?} overlay was not consumed"
        );
    }
    let config = report
        .candidate
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("overlaid synthetic root");
    let rtmp = &config.rtmp_services[0];
    assert_eq!(rtmp.outbound_chunk_size, 4_096);
    assert_eq!(
        rtmp.applications[0].recorders[0].root_directory,
        Path::new("/mnt/storage/recordings")
    );
    let default_404 = config
        .http_services
        .iter()
        .flat_map(|service| &service.routes)
        .find_map(|route| match &route.action {
            HttpRouteAction::StaticFiles {
                error_responses, ..
            } => error_responses
                .iter()
                .find(|response| response.statuses.contains(&404)),
            _ => None,
        })
        .expect("nginx default 404 migration");
    assert_eq!(default_404.body.as_deref().map(str::len), Some(153));
    assert!(
        default_404
            .headers
            .iter()
            .any(|header| header.name == "server" && header.value == "nginx/1.30.2")
    );
}

fn synthetic_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic/nginx-root")
}

fn bearer_overlay(server_name: &str, token_file_path: &str) -> NginxBearerTokenOverlay {
    NginxBearerTokenOverlay {
        server_name: server_name.into(),
        token_file_path: token_file_path.into(),
    }
}

fn bearer_proxy_root() -> &'static str {
    "events {}\nhttp { access_log off; proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; upstream app { server 127.0.0.1:9000; } server { listen 127.0.0.1:18080 default_server; server_name api.example.org; location / { if ($http_authorization != \"Bearer <redacted>\") { return 401; } proxy_pass http://app; } } }\n"
}

fn tls_overlay(
    authority: &str,
    server_name: &str,
    require_connectivity_activation: bool,
) -> NginxUpstreamTlsOverlay {
    NginxUpstreamTlsOverlay {
        authority: authority.into(),
        tls: UpstreamTls {
            server_name: Some(server_name.into()),
            verify: oxiroute_config::UpstreamTlsVerify::Full,
            ca_certificate_path: None,
        },
        require_connectivity_activation,
    }
}

fn explicit_proxy_root(origin: &str) -> String {
    format!(
        "events {{ worker_connections 1024; }}\nhttp {{ access_log off; client_max_body_size 2m; proxy_connect_timeout 15s; proxy_read_timeout 15s; proxy_send_timeout 15s; proxy_http_version 1.1; proxy_buffering off; proxy_request_buffering off; proxy_next_upstream off; proxy_next_upstream_tries 1; proxy_set_header Host $http_host; proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset; server {{ listen 127.0.0.1:18080 default_server; server_name app.example.test; location / {{ proxy_pass {origin}; }} }} }}\n"
    )
}

fn finalized(report: &NginxImportReport) -> &ConfigDraft {
    let Some(config) = report.candidate.validated() else {
        panic!("not finalized: {:#?}", report.diagnostics);
    };
    config.as_draft()
}

fn warning_codes(report: &NginxImportReport) -> Vec<&str> {
    let mut codes = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == Severity::Warning)
        .map(|diagnostic| diagnostic.code().as_str())
        .collect::<Vec<_>>();
    codes.sort_unstable();
    codes
}

fn fixture_root(name: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("fixture directory");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nginx/synthetic")
        .join(name)
        .join("nginx.conf");
    fs::copy(source, directory.path().join("nginx.conf")).expect("copy fixture");
    directory
}

fn audited() -> NginxImportOptions {
    NginxImportOptions {
        x_accel_controls_absent: true,
        ..NginxImportOptions::default()
    }
}

fn import_fixture(name: &str, options: &NginxImportOptions) -> NginxImportReport {
    import_root_with_options(Path::new("nginx.conf"), fixture_root(name).path(), options)
}

fn route_at<'a>(
    routes: &'a [oxiroute_config::HttpRoute],
    prefix: &str,
) -> &'a oxiroute_config::HttpRoute {
    routes
        .iter()
        .find(|route| {
            route.path
                == HttpPathSelector::RawPrefix {
                    value: prefix.into(),
                }
        })
        .unwrap_or_else(|| panic!("no route for {prefix}"))
}

fn proxy_policy(route: &oxiroute_config::HttpRoute) -> &oxiroute_config::HttpProxyPolicy {
    match &route.action {
        HttpRouteAction::Proxy { policy, .. } => policy,
        other => panic!("proxy route expected, got {other:?}"),
    }
}

#[test]
fn acme_webroot_caret_tilde_prefix_lowers_as_a_plain_static_prefix() {
    let report = import_fixture("routes/acme-webroot", &NginxImportOptions::default());
    let routes = &finalized(&report).http_services[0].routes;
    assert_eq!(routes.len(), 4, "two hosts per location");
    assert_eq!(
        routes[0].path,
        HttpPathSelector::RawPrefix {
            value: "/.well-known/acme-challenge/".into()
        }
    );
    let HttpRouteAction::StaticFiles {
        root_directory,
        try_files,
        mime,
        ..
    } = &routes[0].action
    else {
        panic!("static webroot route");
    };
    assert_eq!(root_directory, Path::new("/var/www/letsencrypt"));
    assert_eq!(
        try_files,
        &[
            HttpStaticTryFile::RequestPath,
            HttpStaticTryFile::Status { status: 404 }
        ]
    );
    assert_eq!(mime.default_type.as_deref(), Some("text/plain"));
    assert!(matches!(
        routes[2].action,
        HttpRouteAction::Redirect { status: 301, .. }
    ));
}

#[test]
fn certbot_return_after_an_exact_host_redirect_does_not_duplicate_the_route() {
    let report = import_fixture(
        "routes/certbot-redirect-pair",
        &NginxImportOptions::default(),
    );
    let routes = &finalized(&report).http_services[0].routes;
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].host, None);
    assert!(matches!(
        routes[0].action,
        HttpRouteAction::FixedResponse { status: 404, .. }
    ));
    assert_eq!(
        routes[1].host,
        Some(HttpHostSelector::NormalizedHost {
            value: "a.example.com".into()
        })
    );
    assert!(matches!(
        routes[1].action,
        HttpRouteAction::Redirect { status: 301, .. }
    ));
}

#[test]
fn bearer_gate_accepts_any_literal_and_never_keeps_it() {
    let literal = "test-token-0000";
    let options = NginxImportOptions {
        bearer_tokens: vec![bearer_overlay("api.example.com", "/run/secrets/api.token")],
        ..audited()
    };
    let report = import_fixture("routes/bearer-gate", &options);
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(
        finalized(&report).http_services[0].routes[0].access_policy,
        Some(oxiroute_config::HttpAccessPolicy::BearerTokenFile {
            token_file_path: "/run/secrets/api.token".into(),
            header_name: "authorization".into(),
            realm: None,
        })
    );
    let evidence = format!("{:?}{:?}", report.candidate, report.diagnostics);
    assert!(
        !evidence.contains(literal),
        "the nginx bearer literal leaked into the report"
    );

    let missing = import_fixture("routes/bearer-gate", &audited());
    assert!(missing.has_errors());
    assert!(
        missing
            .candidate
            .operational_overlays
            .iter()
            .any(|overlay| {
                overlay.kind == OperationalOverlayKind::BearerTokenFile
                    && !overlay.satisfied
                    && overlay.redacted_evidence
                    && overlay.values == ["api.example.com"]
            })
    );

    let unmatched = import_fixture(
        "routes/bearer-gate",
        &NginxImportOptions {
            bearer_tokens: vec![bearer_overlay(
                "other.example.com",
                "/run/secrets/other.token",
            )],
            ..audited()
        },
    );
    assert!(unmatched.has_errors());
}

#[test]
fn cookie_path_rewrites_keep_the_full_nginx_replacement_text() {
    let report = import_fixture("proxy/cookie-samesite", &audited());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let routes = &finalized(&report).http_services[0].routes;
    let policy = proxy_policy(route_at(routes, "/plain/"));
    assert_eq!(
        policy.response_cookie_path_rewrites,
        [HttpCookiePathRewrite {
            from: "/".into(),
            to: "/; SameSite=none; Secure".into(),
            mode: CookiePathMatch::NginxPrefix,
        }]
    );
    let flags = proxy_policy(route_at(routes, "/flags/"));
    assert_eq!(flags.response_cookie_path_rewrites, []);
    assert_eq!(flags.response_cookie_attributes.len(), 1);
}

#[test]
fn https_origins_lower_per_bind_scheme_without_scan_overlays() {
    let report = import_fixture("proxy/https-origins", &audited());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = finalized(&report);
    let tls = config
        .upstream_pools
        .iter()
        .map(|pool| pool.tls.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        tls.iter()
            .filter(|tls| **tls == Some(UNVERIFIED_TLS))
            .count(),
        3
    );
    assert_eq!(
        tls.iter().filter(|tls| tls.is_none()).count(),
        1,
        "$scheme on a plain bind stays http"
    );
    assert_eq!(warning_codes(&report), ["W_UPSTREAM_TLS_UNVERIFIED"; 3]);
    assert_eq!(report.candidate.operational_overlays.len(), 0);

    let tls_bind = fixture_root("proxy/https-origins");
    let source = fs::read_to_string(tls_bind.path().join("nginx.conf")).unwrap().replace(
        "listen 192.0.2.10:80 default_server;",
        "listen 192.0.2.10:443 ssl default_server; ssl_certificate /path/to/fullchain.pem; ssl_certificate_key /path/to/privkey.pem; ssl_protocols TLSv1.2 TLSv1.3;",
    );
    fs::write(tls_bind.path().join("nginx.conf"), source).unwrap();
    let secure = import_root_with_options(Path::new("nginx.conf"), tls_bind.path(), &audited());
    assert!(!secure.has_errors(), "{:#?}", secure.diagnostics);
    assert_eq!(
        warning_codes(&secure),
        ["W_UPSTREAM_TLS_UNVERIFIED"; 4].as_slice()
    );
}

#[test]
fn upstream_tls_overlay_on_a_plain_scheme_authority_is_unused_and_blocks() {
    let report = import_fixture(
        "proxy/https-origins",
        &NginxImportOptions {
            upstream_tls: vec![tls_overlay("198.51.100.21", "origin.example.com", false)],
            ..audited()
        },
    );
    assert!(report.has_errors());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("matches no lowered proxy origin")
    }));
}

#[test]
fn host_header_fallback_is_the_first_server_name_verbatim() {
    let report = import_fixture("proxy/hosts", &audited());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let fallbacks = finalized(&report).http_services.iter().map(|service| {
        match &proxy_policy(&service.routes[0]).upstream_host {
            HttpUpstreamHost::NginxHost { fallback } => fallback.as_str(),
            other => panic!("nginx host policy expected, got {other:?}"),
        }
    });
    assert_eq!(
        fallbacks.collect::<Vec<_>>(),
        [".example.org", "*.example.com", "_", ""]
    );
}

#[test]
fn websocket_and_large_upload_policy_lowers_exactly() {
    let report = import_fixture("proxy/ws-timeouts", &audited());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let service = &finalized(&report).http_services[0];
    assert_eq!(service.max_request_body_bytes, Some(1_024 * 1_024));
    let (socket, upload) = (
        route_at(&service.routes, "/ws/"),
        route_at(&service.routes, "/upload/"),
    );
    assert_eq!(
        socket.policy.max_request_body_bytes,
        Some(16_400 * 1_024 * 1_024)
    );
    assert_eq!(socket.policy.read_timeout_ms, 3_600_000);
    assert_eq!(socket.policy.write_timeout_ms, 3_600_000);
    assert!(!socket.policy.request_buffering);
    assert!(upload.policy.request_buffering);
    assert_eq!(upload.policy.read_timeout_ms, 900_000);
    assert_eq!(service.upstream_io_timeout_ms, 3_600_000);
    let connection = |route: &oxiroute_config::HttpRoute| {
        proxy_policy(route)
            .request_headers
            .iter()
            .find_map(|mutation| match mutation {
                HttpRequestHeaderMutation::Set {
                    name,
                    value: HttpRequestHeaderValue::Literal { value },
                } if name.eq_ignore_ascii_case("connection") => Some(value.clone()),
                _ => None,
            })
    };
    assert_eq!(connection(socket).as_deref(), Some("upgrade"));
    assert_eq!(connection(upload).as_deref(), Some("Upgrade"));
}

#[test]
fn default_proxy_buffering_streams_with_one_warning_per_location() {
    let report = import_fixture("proxy/buffering-default", &audited());
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let routes = &finalized(&report).http_services[0].routes;
    assert!(routes.iter().all(|route| !route.policy.response_buffering));
    assert_eq!(warning_codes(&report), ["W_PROXY_BUFFERING_STREAMED"; 2]);
}

fn log_options(path: &str) -> NginxImportOptions {
    NginxImportOptions {
        default_access_log: Some(NginxDefaultAccessLogOverlay { path: path.into() }),
        ..audited()
    }
}

#[test]
fn formatted_access_log_migrates_through_the_overlay_and_lists_dropped_fields() {
    let report = import_fixture(
        "logs/custom-format",
        &log_options("/var/log/oxiroute/access.jsonl"),
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(
        finalized(&report).http_services[0].access_log,
        Some(AccessLogPolicy::File {
            path: "/var/log/oxiroute/access.jsonl".into()
        })
    );
    assert_eq!(warning_codes(&report), ["W_ACCESS_LOG_MIGRATED"]);
    let warning = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity() == Severity::Warning)
        .unwrap();
    assert!(warning.message().ends_with(
        "fields not reproduced: remote_addr, remote_user, request, http_referer, http_user_agent"
    ));
    assert!(report.candidate.operational_overlays.iter().any(|overlay| {
        overlay.kind == OperationalOverlayKind::StructuredAccessLogMigration && overlay.satisfied
    }));
}

#[test]
fn formatted_access_log_without_the_overlay_stays_blocked() {
    let report = import_fixture("logs/custom-format", &audited());
    assert!(report.has_errors());
    assert!(report.candidate.validated().is_none());
}

#[test]
fn access_log_off_stays_disabled_and_mixed_binds_stay_blocked() {
    let directory = fixture_root("logs/custom-format");
    let path = directory.path();
    let source = fs::read_to_string(path.join("nginx.conf")).unwrap();
    let write = |source: &str| fs::write(path.join("nginx.conf"), source).unwrap();
    let import = |options: &NginxImportOptions| {
        import_root_with_options(Path::new("nginx.conf"), path, options)
    };

    write(&source.replace("access_log /var/log/nginx/x.log custom;", "access_log off;"));
    let off = import(&audited());
    let off = finalized(&off).http_services[0].access_log.clone();
    assert_eq!(off, Some(AccessLogPolicy::Disabled));
    let unused = import(&log_options("/var/log/oxiroute/access.jsonl"));
    assert!(
        unused.has_errors(),
        "an overlay with nothing to migrate is an error"
    );

    let second = "server { listen 192.0.2.10:80; server_name b.example.com; access_log off; location / { return 204; } }\n}\n";
    write(&format!(
        "{}{second}",
        source.trim_end().trim_end_matches('}')
    ));
    let mixed = import(&log_options("/var/log/oxiroute/access.jsonl"));
    assert!(mixed.has_errors());
    assert!(mixed.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("cannot mix migrated default and disabled")
    }));
}

#[test]
fn accepted_directives_without_runtime_effect_are_reported_individually() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("nginx.conf"),
        "events {}\nhttp { access_log off; error_log off; sendfile on; send_timeout 600; types_hash_max_size 2048; large_client_header_buffers 4 32k; log_format unused '$host'; limit_conn_zone $binary_remote_addr zone=a:10m; limit_req_zone $binary_remote_addr zone=b:10m rate=1r/s; server { listen 192.0.2.10:80 default_server; location / { return 204; } } }\n"
    )
    .unwrap();
    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions::default(),
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert_eq!(warning_codes(&report), ["W_DIRECTIVE_NO_EFFECT"; 8]);
}
