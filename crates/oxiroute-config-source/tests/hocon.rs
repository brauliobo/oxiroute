use oxiroute_config_source::{ConfigFormat, ConfigSourceError, decode_value, render_value};
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
fn renders_deterministic_json_that_hocon_can_decode() {
    let value = json!({"z": [true, null], "a": {"two": 2, "one": 1}});
    let rendered = render_value(ConfigFormat::Hocon, &value).unwrap();
    assert_eq!(
        rendered,
        concat!(
            "{\n",
            "  \"a\": {\n",
            "    \"one\": 1,\n",
            "    \"two\": 2\n",
            "  },\n",
            "  \"z\": [\n",
            "    true,\n",
            "    null\n",
            "  ]\n",
            "}\n",
        )
    );
    assert_eq!(
        decode_value(ConfigFormat::Hocon, rendered.as_bytes()).unwrap(),
        value
    );
}
