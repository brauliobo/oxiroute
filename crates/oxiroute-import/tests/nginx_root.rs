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
    assert!(blocked.candidate.config.is_none());

    let report = import_root_with_options(
        Path::new("nginx.conf"),
        directory.path(),
        &NginxImportOptions {
            x_accel_controls_absent: true,
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let config = report.candidate.config.expect("audited proxy candidate");
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
                timezone: "America/Bahia".into(),
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    assert!(report.blocked_http_services.is_empty());
    assert!(report.blocked_rtmp_services.is_empty());
    let config = report.candidate.config.as_ref().expect("merged config");
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
        RtmpRecorderTimezone::Iana("America/Bahia".into())
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
    assert!(
        report
            .candidate
            .deployment_requirements
            .iter()
            .any(|requirement| { requirement.kind == DeploymentRequirementKind::ProcessUser })
    );
    assert!(
        report
            .candidate
            .deployment_requirements
            .iter()
            .any(|requirement| { requirement.kind == DeploymentRequirementKind::ProcessGroup })
    );
    assert!(
        report
            .candidate
            .deployment_requirements
            .iter()
            .any(|requirement| { requirement.kind == DeploymentRequirementKind::ModuleLoad })
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
                timezone: "America/Bahia".into(),
            }],
            ..NginxImportOptions::default()
        },
    );
    assert!(unused.candidate.config.is_none());
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
                    timezone: "America/Bahia".into(),
                },
                NginxHostTimezoneOverlay {
                    timezone: "America/Recife".into(),
                },
            ],
            ..NginxImportOptions::default()
        },
    );
    assert!(duplicate.candidate.config.is_none());
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
        dns.candidate.config.as_ref().unwrap().upstream_pools[0].tls,
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
    assert!(ambiguous.candidate.config.is_none());
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
        overlaid.candidate.config.as_ref().unwrap().upstream_pools[0].tls,
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
    assert!(duplicate.candidate.config.is_none());
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
    assert!(misspelled.candidate.config.is_none());
    assert!(misspelled.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .contains("matches no lowered proxy origin")
    }));

    let hostrouter = import_root_with_options(
        Path::new("nginx.conf"),
        &live_fixture("hostrouter"),
        &NginxImportOptions {
            bearer_tokens: vec![
                NginxBearerTokenOverlay {
                    server_name: "ollama.yellowmaverick.com".into(),
                    token_file_path: "/run/secrets/ollama.token".into(),
                },
                NginxBearerTokenOverlay {
                    server_name: "OLLAMA.YELLOWMAVERICK.COM".into(),
                    token_file_path: "/run/secrets/other.token".into(),
                },
            ],
            upstream_tls: live_options("hostrouter").upstream_tls,
            host_timezones: Vec::new(),
            default_access_log: None,
            recording_root: None,
            default_error_page: None,
            x_accel_controls_absent: false,
        },
    );
    assert!(hostrouter.candidate.config.is_none());
    assert!(
        hostrouter
            .candidate
            .operational_overlays
            .iter()
            .any(|overlay| {
                overlay.kind == OperationalOverlayKind::BearerTokenFile && !overlay.satisfied
            })
    );
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
    let config = report.candidate.config.as_ref().expect("scheme config");
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
fn sanitized_live_source_trees_load_as_complete_graphs() {
    for (host, expected_sources) in [("whitebeast", 8), ("hostrouter", 19), ("phoenix", 3)] {
        let directory = live_fixture(host);
        let report = import_root(Path::new("nginx.conf"), &directory);
        assert!(report.source_graph.snapshot_stable, "unstable {host} graph");
        assert_eq!(
            report.source_graph.sources.len(),
            expected_sources,
            "incomplete {host} source graph: {:#?}",
            report.diagnostics
        );
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(directory.parent().unwrap().join("metadata.json"))
                .expect("read live fixture metadata"),
        )
        .expect("parse live fixture metadata");
        assert_eq!(metadata["host"], host);
        assert_eq!(
            metadata["host_timezone"],
            if host == "whitebeast" {
                "America/Recife"
            } else {
                "America/Bahia"
            }
        );
        assert_eq!(metadata["schema_version"], 4);
        assert_eq!(
            metadata["audit_status"],
            "live_origin_hashed_read_only_captured"
        );
        assert_eq!(metadata["sanitized"], true);
        assert_eq!(
            metadata["files"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|file| file["path"].as_str().unwrap().starts_with("nginx/"))
                .count(),
            expected_sources
        );
        assert_eq!(metadata["native_versions"]["nginx"], "1.30.4");
        assert_eq!(metadata["native_versions"]["haproxy"], "3.4.2");
        assert_eq!(metadata["native_version_availability"], "recorded");
        assert_eq!(metadata["origin_captures"].as_array().unwrap().len(), 2);
        assert_eq!(metadata["sanitizer"]["raw_bytes_stored"], false);
    }
}

#[test]
fn sanitized_live_nginx_roots_enforce_security_overlays_and_unrelated_blockers() {
    for host in ["whitebeast", "hostrouter", "phoenix"] {
        let report = import_root_with_options(
            Path::new("nginx.conf"),
            &live_fixture(host),
            &live_options(host),
        );
        if host != "phoenix" {
            assert!(report.has_errors());
            assert!(report.candidate.config.is_none());
            assert!(
                report
                    .candidate
                    .operational_overlays
                    .iter()
                    .any(|overlay| !overlay.satisfied)
            );
            if host == "hostrouter" {
                assert!(
                    report
                        .candidate
                        .operational_overlays
                        .iter()
                        .flat_map(|overlay| &overlay.values)
                        .any(|value| value == "token_file_path=/run/secrets/ollama.token")
                );
            }
            continue;
        }
        assert!(!report.has_errors(), "{:#?}", report.diagnostics);
        assert!(report.candidate.config.is_some());
        assert!(report.candidate.operational_overlays.iter().any(|overlay| {
            overlay.kind == OperationalOverlayKind::StructuredAccessLogMigration
                && overlay.satisfied
        }));
        assert!(report.candidate.operational_overlays.iter().any(|overlay| {
            overlay.kind == OperationalOverlayKind::RecordingRootMigration && overlay.satisfied
        }));
        assert_eq!(
            report.candidate.config.as_ref().unwrap().rtmp_services[0].applications[0].recorders[0]
                .root_directory,
            Path::new("/mnt/cloud/4tb/cam-rtmp")
        );
        let default_404 = report
            .candidate
            .config
            .as_ref()
            .unwrap()
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
                .any(|header| { header.name == "server" && header.value == "nginx/1.30.2" })
        );
        assert_eq!(
            report.candidate.draft.rtmp_services[0].outbound_chunk_size,
            4_096
        );
    }
}

fn live_fixture(host: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/live")
        .join(host)
        .join("nginx")
}

fn live_options(host: &str) -> NginxImportOptions {
    let mut options = NginxImportOptions::default();
    if host == "phoenix" {
        options.host_timezones = vec![NginxHostTimezoneOverlay {
            timezone: "America/Bahia".into(),
        }];
        options.default_access_log = Some(NginxDefaultAccessLogOverlay {
            path: "/var/lib/oxiroute/http-access.jsonl".into(),
        });
        options.recording_root = Some(NginxRecordingRootOverlay {
            path: "/mnt/cloud/4tb/cam-rtmp".into(),
        });
        options.default_error_page = Some(NginxDefaultErrorPageOverlay {
            server: "nginx/1.30.2".into(),
        });
    }
    if host == "hostrouter" {
        options.upstream_tls = vec![
            tls_overlay("10.0.11.211", "phoenix.brauliobo.org", false),
            tls_overlay("phoenix.lan:4081", "phoenix.lan", true),
            tls_overlay("10.0.11.204", "nuvem.d4all.org", false),
        ];
        options.bearer_tokens = vec![NginxBearerTokenOverlay {
            server_name: "ollama.yellowmaverick.com".into(),
            token_file_path: "/run/secrets/ollama.token".into(),
        }];
    }
    options
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
