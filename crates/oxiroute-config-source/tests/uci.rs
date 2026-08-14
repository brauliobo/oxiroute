use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_STRUCTURAL_DEPTH, UciEntry, decode_value,
    parse_uci_document,
};
use serde_json::json;

#[test]
fn public_ast_preserves_lists_for_future_native_mappings() {
    let document = parse_uci_document(
        b"config service 'web'\n\toption enabled '1'\n\tlist listen '80'\n\tlist listen '443'\n",
    )
    .unwrap();
    assert_eq!(document.sections[0].section_type, "service");
    assert_eq!(document.sections[0].name, "web");
    assert_eq!(document.sections[0].option("enabled"), Some("1"));
    assert!(matches!(
        &document.sections[0].entries[2],
        UciEntry::List { name, value } if name == "listen" && value == "443"
    ));
}

#[test]
fn rejects_invalid_uci_syntax_and_duplicate_declarations() {
    let invalid = [
        "config json ''",
        "option kind object",
        "config json root\noption kind object\noption kind array",
        "config json root\nlist kind object\noption kind array",
        "config json root\nconfig json root",
        "config json root\nunknown value",
        "config '' root",
        "config json root\noption '' value",
        "config json 'root",
        "config json root\noption kind \"bad\\q\"",
    ];
    for source in invalid {
        assert!(
            parse_uci_document(source.as_bytes()).is_err(),
            "source was accepted: {source:?}"
        );
    }
}

#[test]
fn rejects_lists_and_unknown_fields_in_generic_records() {
    for source in [
        "config json root\noption kind object\nlist child node",
        "config json root\noption kind object\noption shell command",
    ] {
        assert!(decode_value(ConfigFormat::Uci, source.as_bytes()).is_err());
    }
}

#[test]
fn validates_parent_graph_cycles_orphans_and_unknown_parents() {
    let cases = [
        (
            "cycle",
            concat!(
                "config json root\noption kind object\n",
                "config json a\noption parent b\noption key a\noption kind object\n",
                "config json b\noption parent a\noption key b\noption kind object\n",
            ),
        ),
        (
            "orphan",
            concat!(
                "config json root\noption kind object\n",
                "config json loose\noption kind null\n",
            ),
        ),
        (
            "unknown parent",
            concat!(
                "config json root\noption kind object\n",
                "config json child\noption parent missing\noption key child\noption kind null\n",
            ),
        ),
    ];
    for (case, source) in cases {
        let error = decode_value(ConfigFormat::Uci, source.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ConfigSourceError::Parse { format: "UCI", .. }),
            "unexpected result for {case}: {error}"
        );
    }
}

#[test]
fn validates_object_keys_and_array_indices() {
    let cases = [
        concat!(
            "config json root\noption kind object\n",
            "config json a\noption parent root\noption key same\noption kind null\n",
            "config json b\noption parent root\noption key same\noption kind null\n",
        ),
        concat!(
            "config json root\noption kind array\n",
            "config json a\noption parent root\noption index 0\noption kind null\n",
            "config json b\noption parent root\noption index 0\noption kind null\n",
        ),
        concat!(
            "config json root\noption kind array\n",
            "config json a\noption parent root\noption index 1\noption kind null\n",
        ),
        concat!(
            "config json root\noption kind object\n",
            "config json a\noption parent root\noption index 0\noption kind null\n",
        ),
        concat!(
            "config json root\noption kind array\n",
            "config json a\noption parent root\noption key bad\noption kind null\n",
        ),
    ];
    for source in cases {
        assert!(decode_value(ConfigFormat::Uci, source.as_bytes()).is_err());
    }
}

#[test]
fn decodes_a_hand_authored_generic_document() {
    let source = br"
config json 'root'
  option kind 'object'

config json 'name'
  option parent 'root'
  option key 'name'
  option kind 'string'
  option value 'edge'

config json 'ports'
  option parent 'root'
  option key 'ports'
  option kind 'array'

config json 'port-0'
  option parent 'ports'
  option index '0'
  option kind 'number'
  option value '80'
";
    assert_eq!(
        decode_value(ConfigFormat::Uci, source).unwrap(),
        json!({"name": "edge", "ports": [80]})
    );
}

#[test]
fn applies_the_structural_depth_bound_to_the_record_graph() {
    let mut source = String::from("config json root\noption kind object\n");
    let mut parent = String::from("root");
    for depth in 0..=MAX_STRUCTURAL_DEPTH {
        let name = format!("node-{depth}");
        writeln!(
            source,
            "config json {name}\noption parent {parent}\noption key child\noption kind object"
        )
        .unwrap();
        parent = name;
    }
    assert!(matches!(
        decode_value(ConfigFormat::Uci, source.as_bytes()),
        Err(ConfigSourceError::StructuralDepth)
    ));
}
use std::fmt::Write as _;
