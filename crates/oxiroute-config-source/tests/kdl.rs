use std::fmt::Write as _;

use oxiroute_config_source::{ConfigFormat, ConfigSourceError, MAX_STRUCTURAL_DEPTH, decode_value};
use serde_json::json;

#[test]
fn decodes_the_documented_shape() {
    let source = br#"
version 1
(array)listeners {
  (object)- {
    name "web"
    protocol "http"
  }
}
"#;
    assert_eq!(
        decode_value(ConfigFormat::Kdl, source).unwrap(),
        json!({"version": 1, "listeners": [{"name": "web", "protocol": "http"}]})
    );
}

#[test]
fn rejects_ambiguous_or_non_reversible_kdl_shapes() {
    let invalid = [
        ("property", "node key=1"),
        ("duplicate", "name 1\nname 2\n"),
        ("untyped children", "node { child 1 }"),
        ("container argument", "(object)node 1 { child 2 }"),
        ("container without children", "(array)node"),
        ("bad array name", "(array)items { child 1 }"),
        ("typed scalar", "node (decimal)1"),
        ("unknown node type", "(map)node { child 1 }"),
        ("nonfinite", "node #inf"),
        (
            "oversized integer",
            "node 340282366920938463463374607431768211455",
        ),
    ];
    for (case, source) in invalid {
        assert!(
            matches!(
                decode_value(ConfigFormat::Kdl, source.as_bytes()),
                Err(ConfigSourceError::Parse {
                    format: "KDL 2",
                    ..
                })
            ),
            "case {case} was accepted"
        );
    }
}

#[test]
fn does_not_fall_back_to_kdl_v1() {
    assert!(decode_value(ConfigFormat::Kdl, b"enabled true").is_err());
}

#[test]
fn rejects_kdl_documents_beyond_the_structural_depth_bound() {
    let mut source = String::new();
    for depth in 0..=MAX_STRUCTURAL_DEPTH {
        writeln!(source, "(object)level-{depth} {{").unwrap();
    }
    source.push_str("value #true\n");
    for _ in 0..=MAX_STRUCTURAL_DEPTH {
        source.push_str("}\n");
    }

    assert!(matches!(
        decode_value(ConfigFormat::Kdl, source.as_bytes()),
        Err(ConfigSourceError::StructuralDepth)
    ));
}

#[test]
fn depth_scanner_ignores_braces_inside_literals_and_comments() {
    let source = br##"
quoted "{ }"
raw #"{ }"#
line #true // { }
block #true /* { } */
"##;

    assert!(decode_value(ConfigFormat::Kdl, source).is_ok());
}
