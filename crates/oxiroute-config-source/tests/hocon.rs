use std::fmt::Write as _;

use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_STRUCTURAL_DEPTH, MAX_SUBSTITUTIONS, decode_value,
};
use serde_json::json;

#[test]
fn retains_substitutions_and_object_merging_with_an_empty_environment() {
    let source = br#"
defaults = {
  host = "127.0.0.1"
  nested = { first = 1 }
}
server = ${defaults}
server.nested.second = 2
from_process_environment = ${?HOME}
"#;
    assert_eq!(
        decode_value(ConfigFormat::Hocon, source).unwrap(),
        json!({
            "defaults": {"host": "127.0.0.1", "nested": {"first": 1}},
            "server": {
                "host": "127.0.0.1",
                "nested": {"first": 1, "second": 2}
            }
        })
    );
}

#[test]
fn required_process_environment_substitution_is_unresolved() {
    let error = decode_value(ConfigFormat::Hocon, b"from_process_environment = ${HOME}")
        .unwrap_err()
        .to_string();
    assert!(error.contains("HOME"));
}

#[test]
fn rejects_every_include_form_before_resolution() {
    for source in [
        "include \"missing.conf\"\nvalue = 1",
        "include file(\"missing.conf\")\nvalue = 1",
        "include required(file(\"missing.conf\"))\nvalue = 1",
        "nested { include url(\"https://example.invalid/config\") }",
    ] {
        let error = decode_value(ConfigFormat::Hocon, source.as_bytes()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigSourceError::Parse {
                    format: "HOCON",
                    ..
                }
            ) && error
                .to_string()
                .contains("include directives are forbidden"),
            "unexpected error for {source:?}: {error}"
        );
    }
}

#[test]
fn rejects_hocon_documents_beyond_the_structural_depth_bound() {
    let mut source = String::from("root = ");
    for _ in 0..=MAX_STRUCTURAL_DEPTH {
        source.push_str("{ value = ");
    }
    source.push_str("true");
    for _ in 0..=MAX_STRUCTURAL_DEPTH {
        source.push_str(" }");
    }

    assert!(matches!(
        decode_value(ConfigFormat::Hocon, source.as_bytes()),
        Err(ConfigSourceError::StructuralDepth)
    ));
}

#[test]
fn rejects_hocon_sources_beyond_the_substitution_bound() {
    let mut source = String::from("base = { value = true }\n");
    for index in 0..=MAX_SUBSTITUTIONS {
        writeln!(source, "copy-{index} = ${{base}}").unwrap();
    }

    assert!(matches!(
        decode_value(ConfigFormat::Hocon, source.as_bytes()),
        Err(ConfigSourceError::SubstitutionLimit)
    ));
}
