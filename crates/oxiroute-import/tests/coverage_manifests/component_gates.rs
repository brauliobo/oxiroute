use std::collections::BTreeSet;

use crate::{
    manifests::{ComponentManifest, ComponentStatus},
    support::{read_manifest, read_source, workspace_path},
};

#[test]
fn cache_forward_and_varnish_foundations_are_not_integrated_runtime_claims() {
    let manifest: ComponentManifest = read_manifest("components.json");
    assert_eq!(manifest.schema_version, 1);
    let expected = [
        "component.cache-core",
        "component.forward-proxy-h1",
        "component.forward-proxy-h2",
        "component.forward-proxy-h3",
        "component.varnish-import",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual = manifest
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    for entry in &manifest.entries {
        assert!(entry.gates.component.0, "{} component gate", entry.id);
        assert!(entry.gates.tests.0, "{} tests gate", entry.id);
        if entry.id == "component.forward-proxy-h1" {
            assert_eq!(entry.status, ComponentStatus::Integrated);
            assert!(entry.gates.canonical.0, "{} canonical gate", entry.id);
            assert!(
                entry.gates.integrated_runtime.0,
                "{} integrated runtime gate",
                entry.id
            );
            assert!(entry.gates.failure.0, "{} failure gate", entry.id);
        } else {
            assert_eq!(entry.status, ComponentStatus::Foundation);
            assert!(!entry.gates.canonical.0, "{} canonical gate", entry.id);
            assert!(
                !entry.gates.integrated_runtime.0,
                "{} integrated runtime gate",
                entry.id
            );
        }
    }
    assert!(
        manifest
            .entries
            .iter()
            .find(|entry| entry.id == "component.cache-core")
            .expect("cache component")
            .gates
            .failure
            .0
    );
    assert!(
        manifest
            .entries
            .iter()
            .find(|entry| entry.id == "component.forward-proxy-h3")
            .expect("H3 component")
            .gates
            .failure
            .0
    );

    assert!(workspace_path("crates/oxiroute-cache/Cargo.toml").is_file());
    assert!(workspace_path("crates/oxiroute-forward-proxy/Cargo.toml").is_file());
    let root_workspace = read_source("Cargo.toml");
    assert!(root_workspace.contains("crates/oxiroute-cache"));
    assert!(root_workspace.contains("crates/oxiroute-forward-proxy"));
    let server = read_source("crates/oxiroute-server/Cargo.toml");
    assert!(!server.contains("oxiroute-cache"));
    assert!(server.contains("oxiroute-forward-proxy"));
}
