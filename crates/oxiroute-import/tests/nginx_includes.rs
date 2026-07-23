#![cfg(unix)]

use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::symlink,
    },
    path::Path,
};

use oxiroute_import::{
    DiagnosticStage, E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND, E_SOURCE_CHANGED, E_SOURCE_IO,
    E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE,
    nginx::{IncludeCandidateStatus, NginxLoadLimits, load, load_with_limits},
};
use tempfile::TempDir;

#[test]
fn expands_relative_globs_and_absolute_includes_in_raw_byte_order() {
    let directory = tempdir();
    let snippets = directory.path().join("conf.d");
    fs::create_dir(&snippets).expect("create snippets");
    write(&snippets.join("a.conf"), b"ascii;");
    write(
        &snippets.join(OsString::from_vec(vec![0xff, b'.', b'c', b'o', b'n', b'f'])),
        b"high;",
    );
    let absolute = directory.path().join("absolute.conf");
    write(&absolute, b"absolute;");

    let mut root = b"include conf.d/*.conf;\ninclude ".to_vec();
    root.extend_from_slice(absolute.as_os_str().as_bytes());
    root.extend_from_slice(b";\nroot;");
    write(&directory.path().join("nginx.conf"), &root);

    let report = load(Path::new("nginx.conf"), directory.path());

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().sources.len(), 4);
    assert_eq!(
        expanded_names(report.value()),
        [
            b"ascii".as_slice(),
            b"high".as_slice(),
            b"absolute".as_slice(),
            b"root".as_slice()
        ]
    );
    assert_eq!(report.value().includes.len(), 2);
    assert_eq!(report.value().includes[0].targets.len(), 2);

    let first_glob_path = &report
        .value()
        .source(report.value().includes[0].targets[0])
        .expect("first glob source")
        .canonical_path;
    let second_glob_path = &report
        .value()
        .source(report.value().includes[0].targets[1])
        .expect("second glob source")
        .canonical_path;
    assert!(first_glob_path.as_os_str().as_bytes() < second_glob_path.as_os_str().as_bytes());
}

#[test]
fn empty_globs_succeed_but_exact_missing_includes_fail() {
    let directory = tempdir();
    write(
        &directory.path().join("nginx.conf"),
        b"include absent/*.conf;\nkept;",
    );

    let empty = load(Path::new("nginx.conf"), directory.path());
    assert!(empty.diagnostics().is_empty());
    assert!(empty.value().includes[0].targets.is_empty());
    assert_eq!(expanded_names(empty.value()), [b"kept".as_slice()]);

    write(
        &directory.path().join("nginx.conf"),
        b"include missing.conf;\nkept;",
    );
    let missing = load(Path::new("nginx.conf"), directory.path());
    assert_eq!(missing.diagnostics().len(), 1);
    assert_eq!(missing.diagnostics()[0].code(), E_INCLUDE_NOT_FOUND);
    assert_eq!(missing.diagnostics()[0].stage(), DiagnosticStage::Resolve);
    assert_eq!(
        missing.diagnostics()[0].primary_span(),
        Some(missing.value().includes[0].span)
    );
    assert_eq!(
        missing.value().includes[0].failure,
        Some(E_INCLUDE_NOT_FOUND)
    );
    assert_eq!(expanded_names(missing.value()), [b"kept".as_slice()]);
}

#[test]
fn relative_includes_use_the_resolved_main_config_parent() {
    let directory = tempdir();
    fs::create_dir(directory.path().join("sites")).expect("create sites");
    write(
        &directory.path().join("sites/nginx.conf"),
        b"include shared.conf;",
    );
    write(&directory.path().join("sites/shared.conf"), b"shared;");
    write(&directory.path().join("shared.conf"), b"wrong;");

    let report = load(Path::new("sites/nginx.conf"), directory.path());

    assert!(report.diagnostics().is_empty());
    assert_eq!(expanded_names(report.value()), [b"shared".as_slice()]);
}

#[test]
fn escaped_glob_metacharacters_resolve_literal_filenames() {
    let directory = tempdir();
    write(
        &directory.path().join("nginx.conf"),
        br"include literal\*.conf;",
    );
    write(&directory.path().join("literal*.conf"), b"literal;");

    let report = load(Path::new("nginx.conf"), directory.path());

    assert!(report.diagnostics().is_empty());
    assert_eq!(expanded_names(report.value()), [b"literal".as_slice()]);
    assert_eq!(report.value().includes[0].candidates.len(), 1);
    assert_eq!(
        report.value().includes[0].candidates[0].path,
        directory.path().join("literal*.conf")
    );
}

#[test]
fn unsupported_glob_grammar_is_blocking_instead_of_an_empty_success() {
    for pattern in [
        "matches/[[:digit:]].conf",
        "matches/[[.a.]].conf",
        "matches/[[=a=]].conf",
        "matches/[abc.conf",
        "matches/abc].conf",
        "matches/[z-a].conf",
    ] {
        let directory = tempdir();
        fs::create_dir(directory.path().join("matches")).expect("create matches");
        write(
            &directory.path().join("nginx.conf"),
            format!("include {pattern};").as_bytes(),
        );

        let report = load(Path::new("nginx.conf"), directory.path());

        assert!(
            report.diagnostics().iter().any(|diagnostic| {
                diagnostic.code() == E_UNSUPPORTED_FEATURE
                    && diagnostic.message().contains("glob grammar")
            }),
            "{pattern}: {:?}",
            report.diagnostics()
        );
        assert_eq!(
            report.value().includes[0].failure,
            Some(E_UNSUPPORTED_FEATURE),
            "{pattern}"
        );
        assert!(report.value().expanded_directives.is_empty(), "{pattern}");
    }
}

#[test]
fn directory_enumeration_stops_at_the_glob_work_budget() {
    let directory = tempdir();
    let matches = directory.path().join("matches");
    fs::create_dir(&matches).expect("create matches");
    for name in ["a.conf", "b.conf", "c.conf"] {
        write(&matches.join(name), b"kept;");
    }
    write(
        &directory.path().join("nginx.conf"),
        b"include matches/*.conf;",
    );

    let report = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_glob_work: 2,
            ..NginxLoadLimits::default()
        },
    );

    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_SOURCE_LIMIT && diagnostic.message().contains("glob directory work")
    }));
    assert_eq!(report.value().includes[0].failure, Some(E_SOURCE_LIMIT));
    assert!(report.value().expanded_directives.is_empty());
}

#[test]
fn repeated_includes_parse_once_and_expand_each_occurrence_with_provenance() {
    let directory = tempdir();
    write(
        &directory.path().join("nginx.conf"),
        b"include shared.conf;\ninclude shared.conf;",
    );
    write(&directory.path().join("shared.conf"), b"include leaf.conf;");
    write(&directory.path().join("leaf.conf"), b"leaf;");

    let report = load(Path::new("nginx.conf"), directory.path());

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().sources.len(), 3);
    assert_eq!(
        expanded_names(report.value()),
        [b"leaf".as_slice(), b"leaf".as_slice()]
    );
    assert_eq!(
        report.value().expanded_directives[0]
            .provenance
            .include_stack
            .len(),
        2
    );
    assert_eq!(
        report.value().expanded_directives[1]
            .provenance
            .include_stack
            .len(),
        2
    );
    assert_ne!(
        report.value().expanded_directives[0]
            .provenance
            .include_stack[0]
            .directive_span,
        report.value().expanded_directives[1]
            .provenance
            .include_stack[0]
            .directive_span
    );
    assert_eq!(
        report.value().expanded_directives[0].provenance.source,
        report.value().expanded_directives[1].provenance.source
    );
}

#[test]
fn detects_canonical_cycles_only_on_the_active_expansion_stack() {
    let directory = tempdir();
    write(&directory.path().join("nginx.conf"), b"include child.conf;");
    write(&directory.path().join("child.conf"), b"include nginx.conf;");

    let report = load(Path::new("nginx.conf"), directory.path());

    assert_eq!(report.value().sources.len(), 2);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_INCLUDE_CYCLE);
    assert_eq!(report.diagnostics()[0].stage(), DiagnosticStage::Resolve);
    assert_eq!(report.diagnostics()[0].include_stack().len(), 1);
    assert_eq!(report.value().includes[1].failure, Some(E_INCLUDE_CYCLE));
}

#[test]
fn missing_root_is_a_source_io_error() {
    let directory = tempdir();
    let report = load(Path::new("missing.conf"), directory.path());

    assert!(report.value().root.is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SOURCE_IO);
    assert_eq!(report.diagnostics()[0].stage(), DiagnosticStage::Source);
}

#[test]
fn include_depth_file_and_aggregate_limits_are_enforced() {
    let directory = limit_fixture();

    let depth = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_include_depth: 1,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&depth, "include depth");

    let files = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_source_files: 2,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&files, "source file count");

    let root_len = fs::read(directory.path().join("nginx.conf"))
        .expect("read root")
        .len();
    let child_len = fs::read(directory.path().join("child.conf"))
        .expect("read child")
        .len();
    let aggregate = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_aggregate_source_bytes: root_len + child_len,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&aggregate, "aggregate source size");
}

#[test]
fn file_reads_stop_at_the_configured_per_source_bound() {
    let directory = tempdir();
    let root = b"include child.conf;";
    write(&directory.path().join("nginx.conf"), root);
    write(&directory.path().join("child.conf"), &[b'x'; 64]);

    let report = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_source_bytes: root.len(),
            ..NginxLoadLimits::default()
        },
    );

    assert_limit(&report, "maximum size");
    assert_eq!(report.value().sources.len(), 1);
    assert_eq!(
        report.value().includes[0].candidates[0].status,
        IncludeCandidateStatus::SourceSizeLimit
    );
}

#[test]
fn structural_depth_glob_matches_and_expanded_occurrences_are_bounded() {
    let directory = tempdir();
    write(
        &directory.path().join("nginx.conf"),
        b"outer { nested { hidden; } kept; } root;",
    );
    let depth = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_structural_depth: 1,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&depth, "structural block depth");
    let outer = &depth.value().expanded_directives[0];
    let nested = &outer.children.as_ref().expect("outer children")[0];
    assert!(
        nested
            .children
            .as_ref()
            .expect("bounded nested block")
            .is_empty()
    );

    fs::create_dir(directory.path().join("glob")).expect("create glob directory");
    write(&directory.path().join("glob/a.conf"), b"a;");
    write(&directory.path().join("glob/b.conf"), b"b;");
    write(&directory.path().join("glob/c.conf"), b"c;");
    write(
        &directory.path().join("nginx.conf"),
        b"include glob/*.conf;",
    );
    let glob = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_glob_matches: 2,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&glob, "glob match count");
    assert_eq!(glob.value().includes[0].candidates.len(), 2);
    assert!(glob.value().includes[0].truncated);
    assert_eq!(
        expanded_names(glob.value()),
        [b"a".as_slice(), b"b".as_slice()]
    );

    write(
        &directory.path().join("nginx.conf"),
        b"include shared.conf; include shared.conf;",
    );
    write(&directory.path().join("shared.conf"), b"one; two;");
    let expanded = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_expanded_directives: 3,
            ..NginxLoadLimits::default()
        },
    );
    assert_limit(&expanded, "expanded directive occurrence count");
    assert_eq!(
        expanded_names(expanded.value()),
        [b"one".as_slice(), b"two".as_slice()]
    );
}

#[test]
fn include_graph_preserves_failed_candidates_with_status_and_provenance() {
    let directory = tempdir();
    let matches = directory.path().join("matches");
    fs::create_dir(&matches).expect("create matches");
    write(&matches.join("a.conf"), b"a;");
    symlink("missing-target", matches.join("b.conf")).expect("create broken symlink");
    fs::create_dir(matches.join("c.conf")).expect("create directory candidate");
    write(&matches.join("d.conf"), b"d;");
    write(&matches.join("e.conf"), b"e;");
    write(
        &directory.path().join("nginx.conf"),
        b"include matches/*.conf;",
    );

    let report = load_with_limits(
        Path::new("nginx.conf"),
        directory.path(),
        NginxLoadLimits {
            max_source_files: 3,
            ..NginxLoadLimits::default()
        },
    );
    let candidates = &report.value().includes[0].candidates;

    assert_eq!(candidates.len(), 5);
    assert_eq!(candidates[0].path, matches.join("a.conf"));
    assert!(matches!(
        candidates[0].status,
        IncludeCandidateStatus::Expanded(_)
    ));
    assert_eq!(candidates[1].path, matches.join("b.conf"));
    assert_eq!(
        candidates[1].status,
        IncludeCandidateStatus::CanonicalizeFailed
    );
    assert_eq!(candidates[2].path, matches.join("c.conf"));
    assert_eq!(candidates[2].status, IncludeCandidateStatus::SourceIo);
    assert_eq!(candidates[3].path, matches.join("d.conf"));
    assert!(matches!(
        candidates[3].status,
        IncludeCandidateStatus::Expanded(_)
    ));
    assert_eq!(candidates[4].path, matches.join("e.conf"));
    assert_eq!(
        candidates[4].status,
        IncludeCandidateStatus::SourceFileLimit
    );
    assert!(candidates.iter().all(|candidate| {
        candidate.provenance.source == report.value().root.expect("root source")
            && candidate.provenance.include_stack.is_empty()
    }));
    assert_eq!(report.value().includes[0].failure, Some(E_SOURCE_CHANGED));
}

fn assert_limit(
    report: &oxiroute_import::Report<oxiroute_import::nginx::SourceGraph>,
    message: &str,
) {
    let diagnostic = report
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code() == E_SOURCE_LIMIT)
        .expect("source limit diagnostic");
    assert!(diagnostic.message().contains(message));
}

fn limit_fixture() -> TempDir {
    let directory = tempdir();
    write(&directory.path().join("nginx.conf"), b"include child.conf;");
    write(&directory.path().join("child.conf"), b"include grand.conf;");
    write(&directory.path().join("grand.conf"), b"grand;");
    directory
}

fn expanded_names(graph: &oxiroute_import::nginx::SourceGraph) -> Vec<&[u8]> {
    graph
        .expanded_directives
        .iter()
        .map(|directive| directive.directive.name.value.as_slice())
        .collect()
}

fn write(path: &Path, contents: &[u8]) {
    fs::write(path, contents).expect("write fixture");
}

fn tempdir() -> TempDir {
    tempfile::tempdir().expect("create tempdir")
}
