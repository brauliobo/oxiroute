use std::fs;

use tempfile::tempdir;

use crate::E_SOURCE_CHANGED;

use super::{NginxLoadLimits, load_inner};

#[test]
fn final_recheck_detects_source_changes() {
    let directory = tempdir().expect("tempdir");
    let root = directory.path().join("nginx.conf");
    fs::write(&root, b"before;").expect("write root");

    let report = load_inner(&root, directory.path(), NginxLoadLimits::default(), || {
        fs::write(&root, b"after;").expect("change root");
    });

    assert!(!report.value().snapshot_stable);
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
    );
}

#[test]
fn final_recheck_detects_glob_changes() {
    let directory = tempdir().expect("tempdir");
    let root = directory.path().join("nginx.conf");
    fs::write(&root, b"include *.inc;").expect("write root");
    fs::write(directory.path().join("a.inc"), b"a;").expect("write include");

    let report = load_inner(&root, directory.path(), NginxLoadLimits::default(), || {
        fs::write(directory.path().join("b.inc"), b"b;").expect("add include");
    });

    assert!(!report.value().snapshot_stable);
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
    );
}
