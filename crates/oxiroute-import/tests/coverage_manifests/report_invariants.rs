use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use oxiroute_import::{
    CanonicalProvenance, Diagnostic, Report, Severity, SourceFile, SourceId,
    apache::{ApacheImportReport, import_root as import_apache},
    haproxy::{
        BlockingReason, CanonicalCandidate as HaproxyCandidate, Configuration, DecisionOutcome,
        EffectiveConfiguration, LoadedSource, import_parsed as import_haproxy, parse_sources,
        resolve_parsed,
    },
    nginx::{
        ImportReport as NginxImportReport, OccurrenceDisposition, RtmpImportReport,
        import_http_fragment, import_rtmp_with_timezone,
    },
};
use tempfile::TempDir;

use crate::{
    manifests::{DirectiveForm, DirectiveManifest},
    support::{read_manifest, workspace_path},
};

#[test]
fn native_import_reports_obey_finalization_and_accounting_invariants() {
    let representable = import_nginx_plaintext_supported_fixture();
    assert_import_report_invariants(&representable);
    assert!(!representable.has_errors());
    assert!(representable.blocked_services.is_empty());
    assert!(representable.config.is_some());

    let apache = import_apache_fixture();
    assert_apache_import_report_invariants(&apache);
    assert!(!apache.has_errors(), "{:?}", apache.diagnostics);
    assert!(apache.candidate.config.is_some());

    let partial = import_nginx_fixture("hostrouter-partial.conf");
    assert_import_report_invariants(&partial);
    assert!(!partial.has_errors());
    assert!(partial.blocked_services.is_empty());
    assert!(partial.config.is_some());

    let rtmp_exact = import_rtmp_exact_fixture();
    assert_rtmp_import_report_invariants(&rtmp_exact);
    assert!(rtmp_exact.config.is_some(), "{:?}", rtmp_exact.diagnostics);
    assert!(!rtmp_exact.has_errors());
    assert!(rtmp_exact.blocked_services.is_empty());
    assert_eq!(rtmp_exact.draft.rtmp_services.len(), 1);

    let rtmp_partial = import_rtmp_fixture("phoenix-audited-partial.conf");
    assert_rtmp_import_report_invariants(&rtmp_partial);
    assert!(rtmp_partial.config.is_none());
    assert!(rtmp_partial.has_errors());
    assert_eq!(rtmp_partial.blocked_services.len(), 1);
    assert_eq!(rtmp_partial.draft.rtmp_services.len(), 1);

    for (name, expected_finalized) in [
        ("minimal-representable.cfg", true),
        ("hostrouter-active.cfg", true),
        ("phoenix-dormant.cfg", true),
    ] {
        let parsed = parse_haproxy_fixture(name);
        let resolved = resolve_parsed(parsed.clone());
        assert_haproxy_report_invariants(parsed.value(), resolved.value());
        let lowered = import_haproxy(parsed);
        assert_eq!(
            lowered.value().config.is_some(),
            expected_finalized,
            "{name}: {:?}",
            lowered.diagnostics()
        );
        assert_eq!(
            lowered.value().config.is_none(),
            lowered
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.severity() == Severity::Error),
            "{name} finalization/error invariant"
        );
        assert_unique_provenance_paths(&lowered.value().provenance, name);
    }
}

fn import_apache_fixture() -> ApacheImportReport {
    let directory = TempDir::new().expect("create Apache fixture directory");
    let root = directory.path().join("httpd.conf");
    fs::write(
        &root,
        b"Listen 127.0.0.1:18080\n<VirtualHost 127.0.0.1:18080>\n  ServerName app.example\n  ProxyPass / http://127.0.0.1:3000/\n</VirtualHost>\n",
    )
    .expect("write Apache fixture");
    import_apache(&root)
}

pub(crate) fn parse_haproxy_source(name: &str, contents: &[u8]) -> Report<Configuration> {
    let path = PathBuf::from(name);
    parse_sources(&[LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), contents),
        path,
    }])
}

pub(crate) fn parse_haproxy_fixture(name: &str) -> Report<Configuration> {
    let path = workspace_path("crates/oxiroute-import/tests/fixtures/haproxy").join(name);
    let contents = fs::read(&path)
        .unwrap_or_else(|error| panic!("read HAProxy fixture {}: {error}", path.display()));
    parse_haproxy_source(name, &contents)
}

pub(crate) fn import_nginx_fixture(name: &str) -> NginxImportReport {
    let source_path = workspace_path("crates/oxiroute-import/tests/fixtures/nginx").join(name);
    let source = fs::read(&source_path)
        .unwrap_or_else(|error| panic!("read nginx fixture {}: {error}", source_path.display()));
    let directory = TempDir::new().expect("create nginx fixture directory");
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx fixture root");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

pub(crate) fn import_nginx_plaintext_supported_fixture() -> NginxImportReport {
    let directory = TempDir::new().expect("create nginx plaintext fixture directory");
    let source = "http {\n  access_log off;\n  client_max_body_size 2m;\n  proxy_connect_timeout 15s;\n  proxy_read_timeout 15s;\n  proxy_send_timeout 15s;\n  proxy_http_version 1.1;\n  proxy_buffering off;\n  proxy_request_buffering off;\n  proxy_next_upstream off;\n  proxy_next_upstream_tries 1;\n  proxy_set_header Host $http_host;\n  proxy_ignore_headers X-Accel-Redirect X-Accel-Expires X-Accel-Limit-Rate X-Accel-Buffering X-Accel-Charset;\n  upstream app { server 127.0.0.1:3000; }\n  server {\n    listen 127.0.0.1:18080 default_server;\n    server_name app.example;\n    location / { proxy_pass http://app; }\n  }\n}\n";
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx plaintext fixture");
    import_http_fragment(Path::new("nginx.conf"), directory.path())
}

pub(crate) fn import_rtmp_fixture(name: &str) -> RtmpImportReport {
    let source_path = workspace_path("crates/oxiroute-import/tests/fixtures/nginx").join(name);
    let source = fs::read(&source_path).unwrap_or_else(|error| {
        panic!("read nginx-RTMP fixture {}: {error}", source_path.display())
    });
    let directory = TempDir::new().expect("create nginx-RTMP fixture directory");
    fs::write(directory.path().join("nginx.conf"), source).expect("write nginx-RTMP fixture root");
    import_rtmp_with_timezone(Path::new("nginx.conf"), directory.path(), "America/Bahia")
}

fn import_rtmp_exact_fixture() -> RtmpImportReport {
    let directory = TempDir::new().expect("create exact nginx-RTMP fixture directory");
    let source = "rtmp { server { listen 127.0.0.1:1935; application live { live on; record all; record_path /var/lib/recordings; } } }\n";
    fs::write(directory.path().join("nginx.conf"), source).expect("write exact nginx-RTMP fixture");
    import_rtmp_with_timezone(Path::new("nginx.conf"), directory.path(), "America/Bahia")
}

pub(crate) fn assert_import_report_invariants(report: &NginxImportReport) {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("nginx-directives.json");
    let classified_keys = manifest
        .entries
        .iter()
        .map(|entry| entry.key.as_bytes())
        .collect::<HashSet<_>>();
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len(),
        "nginx ledger must account for every expanded occurrence"
    );
    assert_eq!(
        report
            .occurrence_ledger
            .iter()
            .map(|decision| decision.occurrence)
            .collect::<HashSet<_>>()
            .len(),
        report.occurrence_ledger.len(),
        "nginx ledger occurrence IDs must be unique"
    );
    for decision in &report.occurrence_ledger {
        assert!(
            classified_keys.contains(decision.name.value.as_slice()),
            "parsed nginx directive `{}` has no coverage classification",
            String::from_utf8_lossy(&decision.name.value)
        );
        if let OccurrenceDisposition::Blocking(code) = decision.disposition {
            assert!(report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == code
                    && diagnostic.severity() == Severity::Error
                    && diagnostic.primary_span().is_some()
            }));
        }
    }
}

pub(crate) fn assert_rtmp_import_report_invariants(report: &RtmpImportReport) {
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len(),
        "nginx-RTMP ledger must account for every expanded occurrence"
    );
    assert_eq!(
        report
            .occurrence_ledger
            .iter()
            .map(|decision| decision.occurrence)
            .collect::<HashSet<_>>()
            .len(),
        report.occurrence_ledger.len(),
        "nginx-RTMP ledger occurrence IDs must be unique"
    );
    for decision in &report.occurrence_ledger {
        if let OccurrenceDisposition::Blocking(code) = decision.disposition {
            assert!(report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == code
                    && diagnostic.severity() == Severity::Error
                    && diagnostic.primary_span().is_some()
            }));
        }
    }
    if report.config.is_some() {
        assert!(!report.has_errors());
        assert!(report.blocked_services.is_empty());
    }
    if report.has_errors() || !report.blocked_services.is_empty() {
        assert!(report.config.is_none());
    }
    assert_unique_provenance_paths(&report.provenance, "nginx-RTMP import report");
}

pub(crate) fn assert_apache_import_report_invariants(report: &ApacheImportReport) {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("apache-directives.json");
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len(),
        "Apache ledger must account for every expanded occurrence"
    );
    assert_eq!(
        report
            .occurrence_ledger
            .iter()
            .map(|decision| decision.occurrence)
            .collect::<HashSet<_>>()
            .len(),
        report.occurrence_ledger.len(),
        "Apache ledger occurrence IDs must be unique"
    );
    for decision in &report.occurrence_ledger {
        assert!(
            manifest.entries.iter().any(|entry| {
                entry
                    .key
                    .eq_ignore_ascii_case(&String::from_utf8_lossy(&decision.name.value))
            }),
            "parsed Apache directive `{}` has no coverage classification",
            String::from_utf8_lossy(&decision.name.value)
        );
        if let oxiroute_import::apache::OccurrenceDisposition::Blocking(code) = decision.disposition
        {
            assert!(report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == code
                    && diagnostic.severity() == Severity::Error
                    && diagnostic.primary_span().is_some()
            }));
        }
    }
    assert_unique_provenance_paths(&report.candidate.provenance, "Apache import report");
    if report.candidate.config.is_some() {
        assert!(!report.has_errors());
        assert!(report.blocked_virtual_hosts.is_empty());
    }
    if report.has_errors() || !report.blocked_virtual_hosts.is_empty() {
        assert!(report.candidate.config.is_none());
    }
}

pub(crate) fn assert_haproxy_report_invariants(
    parsed: &Configuration,
    resolved: &EffectiveConfiguration,
) {
    let manifest: DirectiveManifest<DirectiveForm> = read_manifest("haproxy-directives.json");
    let classified_keys = manifest
        .entries
        .iter()
        .map(|entry| entry.key.as_bytes())
        .collect::<HashSet<_>>();
    let occurrences = parsed
        .files
        .iter()
        .map(|file| {
            file.document.preamble.len()
                + file
                    .document
                    .sections
                    .iter()
                    .map(|section| 1 + section.directives.len())
                    .sum::<usize>()
        })
        .sum::<usize>();
    assert_eq!(resolved.ledger.len(), occurrences);
    assert_eq!(
        resolved
            .ledger
            .iter()
            .map(|decision| decision.occurrence)
            .collect::<HashSet<_>>()
            .len(),
        occurrences,
        "HAProxy ledger occurrence IDs must be unique"
    );
    assert!(resolved.ledger.iter().all(|decision| {
        assert!(
            classified_keys.contains(decision.keyword.as_slice()),
            "parsed HAProxy directive `{}` has no coverage classification",
            String::from_utf8_lossy(&decision.keyword)
        );
        !matches!(
            decision.outcome,
            DecisionOutcome::Blocked(BlockingReason::UnconsumedDirective)
        )
    }));
}

fn assert_unique_provenance_paths<T>(provenance: &[CanonicalProvenance<T>], label: &str) {
    let mut paths = HashSet::new();
    for entry in provenance {
        assert!(
            !entry.origins.is_empty(),
            "{label}: {} has no origin",
            entry.path
        );
        assert!(
            paths.insert(entry.path.as_str()),
            "{label}: duplicate canonical provenance path {}",
            entry.path
        );
    }
}

pub(crate) fn assert_diagnostic_message(diagnostics: &[Diagnostic], expected: &str) {
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message().contains(expected)),
        "missing diagnostic containing {expected:?}"
    );
}

pub(crate) fn diagnostic_count(
    diagnostics: &[Diagnostic],
    code: oxiroute_import::DiagnosticCode,
) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}

pub(crate) type HaproxyProbeReports = (
    Report<Configuration>,
    Report<EffectiveConfiguration>,
    Report<HaproxyCandidate>,
);
