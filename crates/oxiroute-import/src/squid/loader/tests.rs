use std::{fs, os::unix::fs::symlink};

use tempfile::tempdir;

use crate::E_SOURCE_CHANGED;

use super::{SquidLoadLimits, glob_matches, load_inner};

#[test]
fn byte_glob_supports_squid_include_shapes() {
    assert!(glob_matches(b"*.conf", b"10-base.conf"));
    assert!(glob_matches(b"[0-9]?.conf", b"10.conf"));
    assert!(!glob_matches(b"*.conf", b".private.conf"));
    assert!(!glob_matches(b"[!0-9]*", b"1.conf"));
}

#[test]
fn final_recheck_detects_root_symlink_retarget() {
    let directory = tempdir().expect("tempdir");
    let original = directory.path().join("original.conf");
    let replacement = directory.path().join("replacement.conf");
    let root = directory.path().join("squid.conf");
    fs::write(&original, b"via off\n").expect("original root");
    fs::write(&replacement, b"via off\n").expect("replacement root");
    symlink(&original, &root).expect("root symlink");

    let report = load_inner(&root, SquidLoadLimits::default(), || {
        fs::remove_file(&root).expect("remove root symlink");
        symlink(&replacement, &root).expect("retarget root symlink");
    });

    assert_changed(
        &report,
        "Squid root path identity changed while the include graph was being loaded",
    );
}

#[test]
fn final_recheck_detects_include_symlink_retarget() {
    let directory = tempdir().expect("tempdir");
    let original = directory.path().join("original.conf");
    let replacement = directory.path().join("replacement.conf");
    let included = directory.path().join("included.conf");
    let root = directory.path().join("squid.conf");
    fs::write(&original, b"via off\n").expect("original include");
    fs::write(&replacement, b"via off\n").expect("replacement include");
    symlink(&original, &included).expect("include symlink");
    fs::write(&root, b"include included.conf\n").expect("root source");

    let report = load_inner(&root, SquidLoadLimits::default(), || {
        fs::remove_file(&included).expect("remove include symlink");
        symlink(&replacement, &included).expect("retarget include symlink");
    });

    assert_changed(
        &report,
        "Squid include path identity changed while the include graph was being loaded",
    );
}

#[test]
fn final_recheck_detects_glob_addition() {
    let directory = tempdir().expect("tempdir");
    let includes = directory.path().join("conf.d");
    let root = directory.path().join("squid.conf");
    fs::create_dir(&includes).expect("include directory");
    fs::write(includes.join("10-base.conf"), b"via off\n").expect("base include");
    fs::write(&root, b"include conf.d/*.conf\n").expect("root source");

    let report = load_inner(&root, SquidLoadLimits::default(), || {
        fs::write(includes.join("20-added.conf"), b"via off\n").expect("added include");
    });

    assert_changed(
        &report,
        "Squid include glob result changed while the include graph was being loaded",
    );
}

#[test]
fn final_recheck_stops_at_configured_work_limit_after_glob_growth() {
    let directory = tempdir().expect("tempdir");
    let include_dir = directory.path().join("conf.d");
    let root = directory.path().join("squid.conf");
    fs::create_dir(&include_dir).expect("include directory");
    fs::write(include_dir.join("10-base.conf"), b"via off\n").expect("base include");
    fs::write(&root, b"include conf.d/*.conf\n").expect("root source");
    let limits = SquidLoadLimits {
        max_glob_work: 1,
        ..SquidLoadLimits::default()
    };

    let report = load_inner(&root, limits, || {
        for ordinal in 0..64 {
            fs::write(
                include_dir.join(format!("20-added-{ordinal:02}.conf")),
                b"via off\n",
            )
            .expect("added include");
        }
    });

    assert_single_glob_change(&report);
}

#[test]
fn final_recheck_stops_at_configured_match_limit_after_glob_growth() {
    let directory = tempdir().expect("tempdir");
    let include_dir = directory.path().join("conf.d");
    let root = directory.path().join("squid.conf");
    fs::create_dir(&include_dir).expect("include directory");
    fs::write(include_dir.join("10-base.conf"), b"via off\n").expect("base include");
    fs::write(&root, b"include conf.d/*.conf\n").expect("root source");
    let limits = SquidLoadLimits {
        max_glob_matches: 1,
        ..SquidLoadLimits::default()
    };

    let report = load_inner(&root, limits, || {
        fs::write(include_dir.join("20-added.conf"), b"via off\n").expect("added include");
    });

    assert_single_glob_change(&report);
}

#[test]
fn final_recheck_detects_glob_removal() {
    let directory = tempdir().expect("tempdir");
    let includes = directory.path().join("conf.d");
    let root = directory.path().join("squid.conf");
    fs::create_dir(&includes).expect("include directory");
    fs::write(includes.join("10-base.conf"), b"via off\n").expect("base include");
    let removed = includes.join("20-removed.conf");
    fs::write(&removed, b"via off\n").expect("removable include");
    fs::write(&root, b"include conf.d/*.conf\n").expect("root source");

    let report = load_inner(&root, SquidLoadLimits::default(), || {
        fs::remove_file(&removed).expect("remove include");
    });

    assert_changed(
        &report,
        "Squid include glob result changed while the include graph was being loaded",
    );
}

#[test]
fn final_recheck_accepts_an_unchanged_source_graph() {
    let directory = tempdir().expect("tempdir");
    let includes = directory.path().join("conf.d");
    let root_target = directory.path().join("root-target.conf");
    let root = directory.path().join("squid.conf");
    let include_target = directory.path().join("include-target.conf");
    let include_link = directory.path().join("included.conf");
    fs::create_dir(&includes).expect("include directory");
    fs::write(&include_target, b"via off\n").expect("include target");
    symlink(&include_target, &include_link).expect("include symlink");
    fs::write(includes.join("10-base.conf"), b"via off\n").expect("glob include");
    fs::write(&root_target, b"include included.conf conf.d/*.conf\n").expect("root target");
    symlink(&root_target, &root).expect("root symlink");

    let report = load_inner(&root, SquidLoadLimits::default(), || {});

    assert!(report.value().snapshot_stable);
    assert!(report.diagnostics().is_empty());
}

fn assert_changed(report: &crate::Report<super::SourceGraph>, message: &str) {
    assert!(!report.value().snapshot_stable);
    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == E_SOURCE_CHANGED && diagnostic.message() == message
    }));
}

fn assert_single_glob_change(report: &crate::Report<super::SourceGraph>) {
    let message = "Squid include glob result changed while the include graph was being loaded";
    assert_changed(report, message);
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.message() == message)
            .count(),
        1
    );
}
