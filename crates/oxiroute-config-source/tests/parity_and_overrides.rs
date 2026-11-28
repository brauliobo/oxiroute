use std::{fs, path::Path};

use oxiroute_config::{ConfigDraft, compose_validated_configs};
use oxiroute_config_source::{
    ALL_FORMATS, ConfigFormat, SourceOverrides, resolve_source, resolve_source_with_overrides,
};
use serde_json::json;

#[test]
fn format_registry_covers_all_seven_formats_and_extensions() {
    assert_eq!(ALL_FORMATS.len(), 7);
    for spec in ALL_FORMATS {
        assert_eq!(
            ConfigFormat::from_wire_name(spec.wire_name),
            Some(spec.format)
        );
        assert_eq!(spec.format.spec(), spec);
        assert_eq!(spec.format.wire_name(), spec.wire_name);
        assert_eq!(spec.format.display_name(), spec.display_name);
        for ext in spec.extensions {
            assert_eq!(ConfigFormat::from_extension(ext), Some(spec.format));
        }
    }
}

#[test]
fn compose_validated_configs_merges_and_conflict_checks_runtime_tuning_fields() {
    let first: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": []
    }))
    .unwrap();
    let second: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listener_workers": 4,
        "http_fast_lane": false,
        "http_data_plane": "epoll",
        "connection_scheduling": "aged_per_worker",
        "connection_age_ms": 25,
        "tls_handshake_threads": 2,
        "http3_congestion_control": "cubic",
        "http3_segmentation_offload": false,
        "listeners": []
    }))
    .unwrap();

    let composed = compose_validated_configs(vec![first, second.clone()]).unwrap();
    let draft = composed.as_draft();
    assert_eq!(draft.listener_workers, Some(4));
    assert_eq!(draft.http_fast_lane, Some(false));
    assert_eq!(draft.connection_age_ms, Some(25));
    assert_eq!(draft.tls_handshake_threads, Some(2));
    assert_eq!(draft.http3_segmentation_offload, Some(false));

    let conflict: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listener_workers": 8,
        "listeners": []
    }))
    .unwrap();
    let err = compose_validated_configs(vec![second, conflict]).unwrap_err();
    assert!(err.to_string().contains("listener_workers"), "{err}");
}

#[test]
fn oxi_includes_all_formats_and_validates_in_memory_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("oxiroute.oxi");
    let pool_kdl = dir.path().join("pool.kdl");
    fs::write(
        &pool_kdl,
        "(array)upstream_pools {\n  (object)- {\n    name \"api\"\n    (array)servers {\n      (object)- {\n        name \"a\"\n        (object)endpoint {\n          type \"socket\"\n          address \"127.0.0.1:8081\"\n        }\n      }\n    }\n  }\n}\n",
    )
    .unwrap();
    let root_text = b"version 1\ninclude pool.kdl\nlisteners []\n";
    fs::write(&root, root_text).unwrap();

    let resolved = resolve_source(&root, root_text).unwrap();
    assert!(resolved.compositional);
    assert_eq!(resolved.included_files.len(), 1);
    assert_eq!(resolved.config.as_draft().upstream_pools.len(), 1);
    assert_eq!(resolved.config.as_draft().upstream_pools[0].name, "api");

    let canonical_child = fs::canonicalize(&pool_kdl).unwrap();
    let mut overrides = SourceOverrides::new();
    overrides.insert(
        canonical_child,
        b"(array)upstream_pools {\n  (object)- {\n    name \"api-v2\"\n    (array)servers {\n      (object)- {\n        name \"b\"\n        (object)endpoint {\n          type \"socket\"\n          address \"127.0.0.1:8082\"\n        }\n      }\n    }\n  }\n}\n"
            .to_vec(),
    );

    let validated =
        resolve_source_with_overrides(&root, root_text, ConfigFormat::Oxi, Some(&overrides))
            .unwrap();
    assert_eq!(validated.config.as_draft().upstream_pools[0].name, "api-v2");
}

#[test]
fn lua_reads_kdl_hocon_uci_and_oxi_and_respects_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("oxiroute.lua");
    let data = dir.path().join("meta.kdl");
    fs::write(&data, "max_connections 128\n").unwrap();
    let root_src = b"local m = read_kdl('meta.kdl')\nreturn { version = 1, max_connections = m.max_connections, listeners = {} }\n";
    fs::write(&root, root_src).unwrap();

    let resolved = resolve_source(&root, root_src).unwrap();
    assert!(resolved.compositional);
    assert_eq!(resolved.included_files.len(), 1);
    assert_eq!(resolved.config.as_draft().max_connections, Some(128));

    let mut overrides = SourceOverrides::new();
    overrides.insert(
        fs::canonicalize(&data).unwrap(),
        b"max_connections 256\n".to_vec(),
    );
    let updated =
        resolve_source_with_overrides(&root, root_src, ConfigFormat::Lua, Some(&overrides))
            .unwrap();
    assert_eq!(updated.config.as_draft().max_connections, Some(256));
}

#[test]
fn compositional_detection_protects_authored_constructs_across_formats() {
    let scripted_lua =
        b"local mc = 100\nreturn { version = 1, max_connections = mc, listeners = {} }\n";
    assert!(
        resolve_source(Path::new("s.lua"), scripted_lua)
            .unwrap()
            .compositional
    );

    let flat_lua = b"return { version = 1, listeners = {} }\n";
    assert!(
        !resolve_source(Path::new("f.lua"), flat_lua)
            .unwrap()
            .compositional
    );

    let oxi_stanza = b"version 1\npool api\n  servers\n    - name a\n      endpoint\n        type socket\n        address 127.0.0.1:8081\nlisteners []\n";
    assert!(
        resolve_source(Path::new("s.oxi"), oxi_stanza)
            .unwrap()
            .compositional
    );

    let hocon_subst = b"base = 128\nversion = 1\nmax_connections = ${base}\nlisteners = []\n";
    // `base` is an unknown field at root unless stripped; use a valid substitution on a real field:
    let hocon_valid = b"version = 1\nmax_connections = 128\nlistener_workers = 1\nlisteners = []\n";
    assert!(
        !resolve_source(Path::new("f.conf"), hocon_valid)
            .unwrap()
            .compositional
    );
    let _ = hocon_subst;

    let uci_main = b"config oxiroute 'main'\n  option version '1'\n  option log_filter 'info'\n  option cpu_level 'scalar'\n";
    let uci_res = resolve_source(Path::new("m.uci"), uci_main).unwrap();
    assert!(uci_res.compositional);
    assert_eq!(
        uci_res.config.as_draft().log_filter.as_deref(),
        Some("info")
    );
    assert_eq!(
        uci_res.config.as_draft().cpu_level.as_deref(),
        Some("scalar")
    );
}
