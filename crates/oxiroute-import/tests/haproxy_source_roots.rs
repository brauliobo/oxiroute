use std::{fs, path::PathBuf};

use oxiroute_import::{
    DiagnosticStage, E_SOURCE_LIMIT, MAX_SOURCE_BYTES, SourceId,
    haproxy::{
        E_SOURCE_IO, HaproxyLoadLimits, RootLoadFailure, RootLoadOutcome, analyze_roots,
        import_roots, load_roots, load_roots_with_limits, parse_roots,
    },
};
use tempfile::tempdir;

#[test]
fn repeated_file_and_directory_roots_preserve_expanded_order_and_occurrences() {
    let temp = tempdir().expect("temporary directory");
    let directory = temp.path().join("conf.d");
    fs::create_dir(&directory).expect("configuration directory");
    let direct = temp.path().join("main.cfg");
    fs::write(&direct, b"global\n").expect("direct source");
    fs::write(directory.join("20-backend.cfg"), b"backend app\n").expect("later source");
    fs::write(directory.join("10-frontend.cfg"), b"frontend public\n").expect("earlier source");
    fs::write(directory.join(".hidden.cfg"), b"listen hidden\n").expect("hidden source");
    fs::write(directory.join("ignored.conf"), b"listen ignored\n").expect("other extension");
    fs::create_dir(directory.join("30-nested.cfg")).expect("nested directory");

    let roots = [direct.clone(), directory.clone(), direct.clone()];
    let report = load_roots(&roots);

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        report
            .value()
            .iter()
            .map(|loaded| loaded.path.clone())
            .collect::<Vec<_>>(),
        [
            direct.clone(),
            directory.join("10-frontend.cfg"),
            directory.join("20-backend.cfg"),
            direct,
        ]
    );
    assert_eq!(
        report
            .value()
            .iter()
            .map(|loaded| loaded.root_ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 1, 2]
    );
    assert_eq!(
        report
            .value()
            .iter()
            .map(|loaded| loaded.file_ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        report
            .value()
            .iter()
            .map(|loaded| loaded.source.id())
            .collect::<Vec<_>>(),
        [
            SourceId::new(0),
            SourceId::new(1),
            SourceId::new(2),
            SourceId::new(3),
        ]
    );
    assert_eq!(
        report.value()[0].source.bytes(),
        report.value()[3].source.bytes()
    );
}

#[test]
fn repeated_occurrences_count_toward_the_shared_source_file_limit() {
    let temp = tempdir().expect("temporary directory");
    let path = temp.path().join("repeated.cfg");
    fs::write(&path, b"global\n").expect("source");
    let limits = HaproxyLoadLimits {
        max_source_bytes: MAX_SOURCE_BYTES,
        max_source_files: 2,
        max_aggregate_source_bytes: usize::MAX,
        max_directory_entries: usize::MAX,
    };

    let report = load_roots_with_limits(&[path.clone(), path.clone(), path], limits);

    assert!(report.value().is_empty());
    assert_eq!(report.value().decisions.len(), 3);
    assert!(matches!(
        report.value().decisions[2].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::SourceFileLimit)
    ));
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SOURCE_LIMIT);
    assert!(report.diagnostics()[0].message().contains("2 occurrences"));
}

#[test]
fn repeated_occurrences_count_toward_the_shared_aggregate_byte_limit() {
    let temp = tempdir().expect("temporary directory");
    let path = temp.path().join("repeated.cfg");
    fs::write(&path, b"global\n").expect("source");
    let limits = HaproxyLoadLimits {
        max_source_bytes: MAX_SOURCE_BYTES,
        max_source_files: usize::MAX,
        max_aggregate_source_bytes: b"global\n".len(),
        max_directory_entries: usize::MAX,
    };

    let report = load_roots_with_limits(&[path.clone(), path], limits);

    assert!(report.value().is_empty());
    assert_eq!(report.value().decisions.len(), 2);
    assert!(matches!(
        report.value().decisions[1].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::AggregateSourceLimit)
    ));
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SOURCE_LIMIT);
    assert!(
        report.diagnostics()[0]
            .message()
            .contains("aggregate source size")
    );
}

#[test]
fn missing_roots_are_fatal_and_later_roots_are_not_attempted() {
    let temp = tempdir().expect("temporary directory");
    let missing = temp.path().join("missing.cfg");
    let present = temp.path().join("present.cfg");
    fs::write(&present, b"global\n").expect("present source");

    let first = load_roots(&[missing.clone(), present.clone()]);
    let second = load_roots(&[missing.clone(), present.clone()]);

    assert_eq!(first, second);
    assert!(first.value().is_empty());
    assert_eq!(first.value().decisions.len(), 2);
    assert!(matches!(
        first.value().decisions[0].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::SourceIo)
    ));
    assert_eq!(first.value().decisions[0].path, missing);
    assert_eq!(first.value().decisions[1].path, present);
    assert_eq!(
        first.value().decisions[1].outcome,
        RootLoadOutcome::NotAttempted
    );
    assert_eq!(first.diagnostics().len(), 1);
    assert_eq!(first.diagnostics()[0].code(), E_SOURCE_IO);
    assert_eq!(first.diagnostics()[0].stage(), DiagnosticStage::Source);
}

#[test]
fn a_failed_later_root_invalidates_the_complete_ordered_load() {
    let temp = tempdir().expect("temporary directory");
    let first = temp.path().join("first.cfg");
    let missing = temp.path().join("missing.cfg");
    let last = temp.path().join("last.cfg");
    fs::write(&first, b"global\n").expect("first source");
    fs::write(&last, b"frontend ignored\n").expect("last source");

    let report = load_roots(&[first, missing, last]);

    assert!(report.has_errors());
    assert!(report.value().is_empty());
    assert_eq!(report.value().decisions.len(), 3);
    assert!(matches!(
        report.value().decisions[0].outcome,
        RootLoadOutcome::Loaded { file_count: 1 }
    ));
    assert!(matches!(
        report.value().decisions[1].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::SourceIo)
    ));
    assert_eq!(
        report.value().decisions[2].outcome,
        RootLoadOutcome::NotAttempted
    );
}

#[test]
fn fatal_root_decisions_reach_resolution_without_parsing_a_partial_configuration() {
    let temp = tempdir().expect("temporary directory");
    let first = temp.path().join("first.cfg");
    let missing = temp.path().join("missing.cfg");
    let last = temp.path().join("last.cfg");
    fs::write(&first, b"global\n").expect("first source");
    fs::write(&last, b"frontend ignored\n").expect("last source");

    let parsed = parse_roots(&[first.clone(), missing.clone(), last.clone()]);
    assert!(parsed.has_errors());
    assert!(parsed.value().files.is_empty());
    assert_eq!(parsed.value().root_decisions.len(), 3);

    let resolved = analyze_roots(&[first.clone(), missing.clone(), last.clone()]);
    assert!(resolved.has_errors());
    assert!(resolved.value().global.sections.is_empty());
    assert_eq!(resolved.value().root_decisions.len(), 3);
    assert!(matches!(
        resolved.value().root_decisions[1].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::SourceIo)
    ));
    assert_eq!(
        resolved.value().root_decisions[2].outcome,
        RootLoadOutcome::NotAttempted
    );

    let imported = import_roots(&[first, missing, last]);
    assert!(imported.has_errors());
    assert!(imported.value().config().is_none());
    assert!(
        imported
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_IO)
    );
}

#[test]
fn directory_enumeration_limit_fails_before_collecting_or_sorting_the_root() {
    let temp = tempdir().expect("temporary directory");
    let directory = temp.path().join("many-entries");
    let later = temp.path().join("later.cfg");
    fs::create_dir(&directory).expect("configuration directory");
    fs::write(directory.join("a.cfg"), b"global\n").expect("first entry");
    fs::write(directory.join("ignored.txt"), b"ignored\n").expect("second entry");
    fs::write(directory.join("z.cfg"), b"backend app\n").expect("excess entry");
    fs::write(&later, b"frontend later\n").expect("later root");
    let limits = HaproxyLoadLimits {
        max_source_bytes: MAX_SOURCE_BYTES,
        max_source_files: usize::MAX,
        max_aggregate_source_bytes: usize::MAX,
        max_directory_entries: 2,
    };

    let report = load_roots_with_limits(&[directory, later], limits);

    assert!(report.has_errors());
    assert!(report.value().is_empty());
    assert!(matches!(
        report.value().decisions[0].outcome,
        RootLoadOutcome::Failed(RootLoadFailure::DirectoryEntryLimit)
    ));
    assert_eq!(
        report.value().decisions[1].outcome,
        RootLoadOutcome::NotAttempted
    );
    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_SOURCE_LIMIT && diagnostic.message().contains("directory entry work")
    }));
}

#[cfg(unix)]
#[test]
fn unreadable_file_roots_report_source_errors() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("temporary directory");
    let unreadable = temp.path().join("unreadable.cfg");
    fs::write(&unreadable, b"global\n").expect("unreadable source");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
        .expect("remove read permission");

    let report = load_roots(std::slice::from_ref(&unreadable));

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600))
        .expect("restore read permission");
    assert!(report.value().is_empty());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SOURCE_IO);
    assert!(report.diagnostics()[0].message().contains("cannot read"));
}

#[test]
fn source_roots_enforce_the_shared_per_source_byte_bound() {
    let temp = tempdir().expect("temporary directory");
    let oversized = temp.path().join("oversized.cfg");
    fs::write(&oversized, vec![b' '; MAX_SOURCE_BYTES + 1]).expect("oversized source");

    let report = load_roots(std::slice::from_ref(&oversized));

    assert!(report.value().is_empty());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), E_SOURCE_LIMIT);
    assert_eq!(report.diagnostics()[0].stage(), DiagnosticStage::Source);
}

#[test]
fn loaded_occurrences_are_immutable_snapshots_when_files_change() {
    let temp = tempdir().expect("temporary directory");
    let path = temp.path().join("changing.cfg");
    fs::write(&path, b"frontend before\n").expect("initial source");

    let report = load_roots(std::slice::from_ref(&path));
    fs::write(&path, b"backend after\n").expect("changed source");

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value()[0].source.bytes(), b"frontend before\n");
    assert_eq!(fs::read(path).expect("changed bytes"), b"backend after\n");
}

#[cfg(unix)]
#[test]
fn directory_names_are_sorted_by_raw_filename_bytes() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let temp = tempdir().expect("temporary directory");
    for name in [
        b"z.cfg".to_vec(),
        vec![0x80, b'.', b'c', b'f', b'g'],
        vec![0xff, b'.', b'c', b'f', b'g'],
    ] {
        fs::write(temp.path().join(OsString::from_vec(name)), b"global\n")
            .expect("byte-named source");
    }

    let report = load_roots(&[PathBuf::from(temp.path())]);
    let names = report
        .value()
        .iter()
        .map(|loaded| {
            loaded
                .path
                .file_name()
                .expect("selected filename")
                .as_encoded_bytes()
                .to_vec()
        })
        .collect::<Vec<_>>();
    let source_paths = report
        .value()
        .iter()
        .map(|loaded| loaded.source.path().expect("filesystem source path"))
        .collect::<Vec<_>>();

    assert!(report.diagnostics().is_empty());
    assert_eq!(
        names,
        [
            b"z.cfg".to_vec(),
            vec![0x80, b'.', b'c', b'f', b'g'],
            vec![0xff, b'.', b'c', b'f', b'g'],
        ]
    );
    assert_eq!(
        source_paths,
        report
            .value()
            .iter()
            .map(|loaded| loaded.path.as_path())
            .collect::<Vec<_>>()
    );
    assert_ne!(
        report.value()[1].source.name(),
        report.value()[2].source.name()
    );
}
