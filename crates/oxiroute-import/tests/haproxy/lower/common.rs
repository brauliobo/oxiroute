use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use oxiroute_import::{
    Diagnostic, E_SEMANTICS_NOT_REPRESENTABLE, Report, Severity, SourceFile, SourceId,
    haproxy::{
        CanonicalCandidate, E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION,
        E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, HaproxyImportOptions, LoadedSource,
        PreprocessingEnvironment, analyze_sources, import_roots_with_options, import_sources,
    },
};

pub(super) const MINIMAL: &[u8] =
    include_bytes!("../../fixtures/haproxy/minimal-representable.cfg");

pub(super) fn synthetic_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic/haproxy")
        .join(name)
}

pub(super) fn import_synthetic(name: &str, gpu1_defined: bool) -> Report<CanonicalCandidate> {
    let environment = PreprocessingEnvironment {
        node_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
        gpu1_defined,
    };
    let report = import_roots_with_options(
        &[synthetic_path(name)],
        environment,
        &HaproxyImportOptions::default(),
    );
    for code in [
        E_UNKNOWN_DIRECTIVE,
        E_UNCONSUMED_DIRECTIVE,
        E_ENVIRONMENT_EXPANSION,
        E_CONDITIONAL_PREPROCESSING,
    ] {
        assert_eq!(
            code_count(report.diagnostics(), code),
            0,
            "{name}: {:#?}",
            report.diagnostics()
        );
    }
    assert!(
        report.value().validated().is_some(),
        "{name}: {:#?}",
        report.diagnostics()
    );
    assert!(
        report
            .value()
            .source_metadata
            .environment_fingerprint_sha256
            .is_some()
    );
    report
}

pub(super) fn loaded_fixture(name: &str, contents: &[u8]) -> LoadedSource {
    let path = PathBuf::from(name);
    LoadedSource {
        root_ordinal: 0,
        file_ordinal: 0,
        source: SourceFile::from_path(SourceId::new(0), path.clone(), contents),
        path,
    }
}

pub(super) fn analyze_fixture(
    name: &str,
    contents: &[u8],
) -> Report<oxiroute_import::haproxy::EffectiveConfiguration> {
    analyze_sources(&[loaded_fixture(name, contents)])
}

pub(super) fn import_fixture(name: &str, contents: &[u8]) -> Report<CanonicalCandidate> {
    import_sources(&[loaded_fixture(name, contents)])
}

pub(super) fn assert_blocker(diagnostics: &[Diagnostic], message: &str) {
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == E_SEMANTICS_NOT_REPRESENTABLE
                && diagnostic.severity() == Severity::Error
                && diagnostic.primary_span().is_some()
                && diagnostic.message().contains(message)
        }),
        "missing blocker containing {message:?}"
    );
}

pub(super) fn assert_has_provenance(candidate: &CanonicalCandidate, path: &str) {
    assert!(
        candidate
            .provenance
            .iter()
            .any(|provenance| provenance.path == path),
        "missing provenance for {path}"
    );
}

pub(super) fn validated_config(candidate: &CanonicalCandidate) -> &oxiroute_config::ConfigDraft {
    candidate
        .validated()
        .expect("candidate must be validated")
        .as_draft()
}

pub(super) fn diagnostic_contains(diagnostics: &[Diagnostic], message: &str) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message().contains(message))
}

pub(super) fn code_count(
    diagnostics: &[Diagnostic],
    code: oxiroute_import::DiagnosticCode,
) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}
