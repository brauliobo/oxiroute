use std::{ffi::OsString, fs, path::Path};

use oxiroute_import::squid::{
    PrivacyDirective, RootSelectionSource, discover_root, import_selected,
};
use tempfile::tempdir;

use super::common::{assert_squid_origin, assert_squid_provenance_paths_are_unique};

#[test]
fn selected_native_root_report_retains_canonical_provenance() {
    let directory = tempdir().expect("Squid fixture directory");
    let root = directory.path().join("active.conf");
    fs::write(
        &root,
        b"http_port 3128\n\
          access_log none\n\
          forwarded_for delete\n\
          via off\n\
          acl ssl_ports port 443\n\
          http_access deny CONNECT !ssl_ports\n\
          http_access allow all\n",
    )
    .expect("active source");

    let report = import_selected(
        &[OsString::from("-f"), root.clone().into_os_string()],
        Path::new("/synthetic/default.conf"),
    );
    assert_eq!(report.selection.active_root, root);
    assert!(
        report
            .import
            .candidate
            .validated()
            .map(oxiroute_config::ValidatedConfig::as_draft)
            .is_some()
    );
    assert_squid_provenance_paths_are_unique(&report.import);
    assert_squid_origin(&report.import, "/listeners/0", &root, 1, 0);
}

#[test]
fn native_cli_root_discovery_uses_the_last_f_argument() {
    let arguments = [
        OsString::from("--foreground"),
        OsString::from("-f"),
        OsString::from("/tmp/synthetic-first.conf"),
        OsString::from("-f/tmp/synthetic-active.conf"),
        OsString::from("-sYC"),
    ];
    let report = discover_root(&arguments, std::path::Path::new("/etc/squid/squid.conf"));
    assert!(report.diagnostics().is_empty());
    assert_eq!(report.value().command_line_roots.len(), 2);
    assert_eq!(
        report.value().active_root,
        std::path::Path::new("/tmp/synthetic-active.conf")
    );
    assert_eq!(
        report.value().source,
        RootSelectionSource::CommandLine { argument_index: 3 }
    );

    let default = discover_root(
        &[OsString::from("--foreground"), OsString::from("-sYC")],
        std::path::Path::new("/etc/squid/squid.conf"),
    );
    assert_eq!(default.value().source, RootSelectionSource::CompiledDefault);
}

#[test]
fn native_cli_import_preserves_selected_root_and_semantic_report() {
    let directory = tempdir().expect("temp directory");
    let first = directory.path().join("first.conf");
    let active = directory.path().join("active.conf");
    fs::write(&first, b"via on\n").expect("first source");
    fs::write(&active, b"via off\n").expect("active source");
    let arguments = [
        OsString::from("-f"),
        first.into_os_string(),
        OsString::from("-f"),
        active.clone().into_os_string(),
    ];

    let report = import_selected(&arguments, std::path::Path::new("/synthetic/default.conf"));
    assert_eq!(report.selection.active_root, active);
    assert_eq!(report.import.decision_ledger.decisions.len(), 1);
    assert!(matches!(
        report.import.effective.privacy[0],
        PrivacyDirective::Via { enabled: false, .. }
    ));
}
