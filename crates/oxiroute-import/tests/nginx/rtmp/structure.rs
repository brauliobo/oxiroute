use std::{fs, path::Path};

use oxiroute_import::{
    DiagnosticStage, E_DUPLICATE_IDENTITY, E_INVALID_VALUE, E_UNSUPPORTED_FEATURE,
    nginx::{OccurrenceDisposition, import_rtmp_with_timezone, load, resolve_rtmp},
};
use tempfile::TempDir;

use super::support::import_source;

#[test]
fn includes_are_transparent_and_parent_policy_after_the_include_is_inherited() {
    let report = import_source(
        br"
        rtmp {
          server {
            include applications.conf;
            listen 127.0.0.1:1935;
            live on;
            idle_streams off;
          }
        }
        ",
        &[(
            "applications.conf",
            br"application included { record all; record_path /var/lib/included; }",
        )],
    );

    let config = report
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("included RTMP application");
    let application = &config.rtmp_services[0].applications[0];
    assert!(application.live);
    assert!(!application.idle_streams);
    let include = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"include")
        .expect("include decision");
    assert_eq!(include.disposition, OccurrenceDisposition::Structural);
    let application_decision = report
        .occurrence_ledger
        .iter()
        .find(|decision| decision.name.value == b"application")
        .expect("included application decision");
    assert_eq!(application_decision.provenance.include_stack.len(), 1);
}

#[test]
fn separate_entry_ignores_http_semantics_and_lowers_global_rtmp_auto_push() {
    let separate = import_source(
        br"http { server { listen 80; location / { proxy_pass http://backend; } } } rtmp { server { listen 1935; application app {} } }",
        &[],
    );
    assert!(
        separate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some()
    );
    assert!(separate.occurrence_ledger.iter().any(|decision| {
        decision.name.value == b"proxy_pass"
            && decision.disposition == OccurrenceDisposition::Structural
    }));

    let global = import_source(
        br"rtmp_auto_push on; rtmp { server { listen 1935; application app {} } }",
        &[],
    );
    let config = global
        .validated()
        .map(oxiroute_config::ValidatedConfig::as_draft)
        .expect("global auto-push policy");
    assert!(global.blocked_services.is_empty());
    assert!(config.rtmp_services[0].auto_push.enabled);
}

#[test]
fn duplicates_and_overlapping_listens_are_terminal_blockers() {
    for source in [
        br"rtmp { server { listen 1935; application app { live on; live off; } } }".as_slice(),
        br"rtmp { server { listen 0.0.0.0:1935; application one {} } server { listen 127.0.0.1:1935; application two {} } }".as_slice(),
        br"rtmp { server { listen 1935; application same {} application same {} } }".as_slice(),
    ] {
        let report = import_source(source, &[]);
        assert!(report.validated().map(oxiroute_config::ValidatedConfig::as_draft).is_none());
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == E_DUPLICATE_IDENTITY
                && diagnostic.stage() == DiagnosticStage::Resolve
        }));
    }
}

#[test]
fn keeps_supported_servers_in_the_draft_without_placeholder_for_a_blocked_server() {
    let directory = fixture("synthetic/nginx/rtmp-partial-blocked-server.conf");
    let report = import_rtmp_with_timezone(
        Path::new("nginx.conf"),
        directory.path(),
        "America/Argentina/Buenos_Aires",
    );

    assert!(
        report
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_none()
    );
    assert_eq!(report.blocked_services.len(), 1);
    assert_eq!(report.summary().rtmp_services, 1);
    assert_eq!(report.summary().listeners, 1);
    assert!(report.provenance.iter().any(|entry| {
        entry.path == "/rtmp_services/0/applications/0" && !entry.origins.is_empty()
    }));
    assert_eq!(
        report.occurrence_ledger.len(),
        report.source_graph.expanded_occurrences.len()
    );
}

#[test]
fn resolve_entry_point_preserves_every_terminal_occurrence_decision() {
    let directory = TempDir::new().expect("RTMP semantic directory");
    fs::write(
        directory.path().join("nginx.conf"),
        b"http { server { listen 80; } } rtmp { server { listen 1935; application app { mystery on; } } }",
    )
    .expect("write semantic source");
    let graph = load(Path::new("nginx.conf"), directory.path());
    let occurrence_count = graph.value().expanded_occurrences.len();
    let resolved = resolve_rtmp(graph);

    assert_eq!(resolved.value().decisions.len(), occurrence_count);
    assert!(
        resolved
            .value()
            .decisions
            .iter()
            .enumerate()
            .all(|(index, decision)| { decision.occurrence.get() == index })
    );
    assert!(resolved.value().decisions.iter().any(|decision| {
        decision.name.value == b"mystery"
            && decision.disposition == OccurrenceDisposition::Blocking(E_UNSUPPORTED_FEATURE)
    }));
}

#[test]
fn registry_validation_rejects_invalid_values_and_statement_block_shapes() {
    for source in [
        b"rtmp { server { application live { hls_fragment_naming random; } } }".as_slice(),
        b"rtmp;".as_slice(),
        b"rtmp { server { live {} } }".as_slice(),
    ] {
        let report = import_source(source, &[]);
        assert!(
            report
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none()
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == E_INVALID_VALUE),
            "missing registered syntax diagnostic for {}",
            String::from_utf8_lossy(source)
        );
    }
}

fn fixture(name: &str) -> TempDir {
    let directory = TempDir::new().expect("RTMP fixture directory");
    fs::copy(
        Path::new("tests/fixtures").join(name),
        directory.path().join("nginx.conf"),
    )
    .expect("copy RTMP fixture");
    directory
}
