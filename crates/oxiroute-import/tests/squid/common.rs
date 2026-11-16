use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub(super) fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic/squid")
        .join(name)
}

#[allow(clippy::naive_bytecount)]
pub(super) fn assert_squid_provenance_paths_are_unique(
    report: &oxiroute_import::squid::ImportReport,
) {
    let mut paths = HashSet::new();
    assert!(!report.candidate.provenance.is_empty());
    for entry in &report.candidate.provenance {
        assert!(
            paths.insert(entry.path.as_str()),
            "duplicate path {}",
            entry.path
        );
        assert!(!entry.origins.is_empty(), "{} lacks origins", entry.path);
        for origin in &entry.origins {
            let source = report
                .source_graph
                .source(origin.provenance.source)
                .expect("canonical provenance source");
            assert_eq!(origin.directive_span.source(), origin.provenance.source);
            assert!(source.source.path().is_some());
            assert!(
                source.source.bytes()[..origin.directive_span.range().start()]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
                    < source.source.len()
            );
        }
    }
}

#[allow(clippy::naive_bytecount)]
pub(super) fn assert_squid_origin(
    report: &oxiroute_import::squid::ImportReport,
    path: &str,
    expected_source: &Path,
    expected_line: usize,
    expected_include_depth: usize,
) {
    let entry = report
        .candidate
        .provenance
        .iter()
        .find(|entry| entry.path == path)
        .unwrap_or_else(|| panic!("missing Squid provenance {path}"));
    let expected_source = fs::canonicalize(expected_source).expect("canonical source path");
    assert!(entry.origins.iter().any(|origin| {
        let source = report
            .source_graph
            .source(origin.provenance.source)
            .expect("origin source");
        let line = source.source.bytes()[..origin.directive_span.range().start()]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1;
        source.canonical_path == expected_source
            && line == expected_line
            && origin.provenance.include_stack.len() == expected_include_depth
    }));
}
