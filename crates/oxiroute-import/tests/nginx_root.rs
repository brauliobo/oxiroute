#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::{
    AccessLogPolicy, HttpRouteAction, RtmpRecorderSegmentNaming, RtmpRecorderTimeBasis,
    RtmpRecorderTimezone, UpstreamEndpoint, UpstreamTls,
};
use oxiroute_import::{
    DeploymentRequirementKind, OperationalOverlayKind,
    nginx::{
        NginxBearerTokenOverlay, NginxDefaultAccessLogOverlay, NginxDefaultErrorPageOverlay,
        NginxHostTimezoneOverlay, NginxImportOptions, NginxRecordingRootOverlay,
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

#[test]
fn verified_https_derives_dns_sni_and_rejects_ip_ambiguity_without_overlay() {
    let directory = tempfile::tempdir().expect("HTTPS nginx root directory");
    fs::write(
        directory.path().join("nginx.conf"),
        explicit_proxy_root("https://origin.example.test:8443"),
    )
    .expect("write DNS HTTPS root");
    let dns = import_root(Path::new("nginx.conf"), directory.path());
    assert!(!dns.has_errors(), "{:?}", dns.diagnostics);
    assert_eq!(
        dns.candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .upstream_pools[0]
            .tls,
        Some(UpstreamTls {
            server_name: "origin.example.test".into(),
            ca_certificate_path: None,
        })
    );

    fs::write(
        directory.path().join("nginx.conf"),
        explicit_proxy_root("https://192.0.2.44"),
    )
    .expect("write IP HTTPS root");
    let ambiguous = import_root(Path::new("nginx.conf"), directory.path());
    assert!(ambiguous.has_errors());
    assert!(
        ambiguous
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert!(ambiguous.root_occurrence_ledger.iter().any(|decision| {
        matches!(decision.disposition, RootOccurrenceDisposition::Blocking(_))
    }));
    assert!(
        ambiguous
            .candidate
            .operational_overlays
            .iter()
            .any(|overlay| { overlay.kind == OperationalOverlayKind::UpstreamTlsPolicy })
    );

    let overlaid = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            upstream_tls: vec![NginxUpstreamTlsOverlay {
                authority: "192.0.2.44".into(),
                tls: UpstreamTls {
                    server_name: "verified-origin.example.test".into(),
                    ca_certificate_path: None,
                },
                require_connectivity_activation: true,
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(!overlaid.has_errors(), "{:?}", overlaid.diagnostics);
    assert_eq!(
        overlaid
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .unwrap()
            .upstream_pools[0]
            .tls,
        Some(UpstreamTls {
            server_name: "verified-origin.example.test".into(),
            ca_certificate_path: None,
        })
    );
    assert!(overlaid.root_occurrence_ledger.iter().any(|decision| {
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
                    server_name: "scheme-origin.example.test".into(),
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
            server_name: server_name.into(),
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
