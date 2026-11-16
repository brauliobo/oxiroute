use std::fs;

use oxiroute_import::squid::{PeerOption, SecretKind, SemanticBlockerKind, import};
use tempfile::tempdir;

#[test]
fn cache_peer_credentials_become_typed_secret_facts() {
    let directory = tempdir().expect("temp directory");
    let root = directory.path().join("squid.conf");
    fs::write(
        &root,
        b"cache_peer peer.example.test parent 3128 0 login=synthetic-user:synthetic-password token=synthetic-token no-query\n",
    )
    .expect("root source");

    let report = import(&root);
    let peer = &report.effective.cache_peers[0];
    assert!(matches!(
        peer.options[0],
        PeerOption::Secret(secret) if secret.kind == SecretKind::PeerCredentials
    ));
    assert!(matches!(
        peer.options[1],
        PeerOption::Secret(secret) if secret.kind == SecretKind::BearerToken
    ));
    assert!(matches!(peer.options[2], PeerOption::NoQuery));
    let typed_peer = format!("{peer:?}");
    assert!(!typed_peer.contains("synthetic-password"));
    assert!(!typed_peer.contains("synthetic-token"));
    assert!(
        report
            .blocked_capabilities
            .iter()
            .any(|blocked| blocked.kind == SemanticBlockerKind::CachePeerHierarchy)
    );
}

#[test]
fn unsupported_peer_and_direct_forms_keep_stable_blockers() {
    let directory = tempdir().expect("fixture directory");
    let base =
        "http_port 3128\naccess_log none\nforwarded_for delete\nvia off\nhttp_access allow all\n";
    for (name, extra, expected_message) in [
        (
            "sibling",
            "cache_peer peer.example.test sibling 3128 0\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "option",
            "cache_peer peer.example.test parent 3128 0 no-query\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "icp",
            "cache_peer peer.example.test parent 3128 3130\n",
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options",
        ),
        (
            "conditional-direct",
            "acl target dstdomain example.test\nalways_direct allow target\n",
            "Squid direct-routing form is outside the exact global direct-fallback subset",
        ),
    ] {
        let root = directory.path().join(format!("{name}.conf"));
        fs::write(&root, format!("{base}{extra}")).expect("unsupported source");
        let report = import(&root);
        assert!(
            report
                .candidate
                .validated()
                .map(oxiroute_config::ValidatedConfig::as_draft)
                .is_none(),
            "{name} unexpectedly lowered"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message() == expected_message }),
            "{name} lacks stable blocker message"
        );
    }
}
