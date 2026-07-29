use oxiroute_config_source::{
    ConfigSourceError, MAX_EXPANSION_DEPTH, MAX_STRING_BYTES, expand_templates,
};
use serde_json::{Map, Value, json};

#[test]
fn recursively_merges_templates_with_local_precedence() {
    let source = json!({
        "templates": {
            "base": {
                "nested": {"first": 1, "replace": "base"},
                "array": [1, 2],
                "nullable": "base"
            },
            "derived": {
                "use": "base",
                "nested": {"second": 2, "replace": "derived"},
                "array": [3]
            },
            "last": {"nested": {"third": 3}, "last": true}
        },
        "item": {
            "use": ["derived", "last"],
            "nested": {"replace": "local"},
            "nullable": null
        }
    });
    assert_eq!(
        expand_templates(&source).unwrap(),
        json!({
            "item": {
                "nested": {
                    "first": 1,
                    "second": 2,
                    "third": 3,
                    "replace": "local"
                },
                "array": [3],
                "nullable": null,
                "last": true
            }
        })
    );
}

#[test]
fn expands_nested_uses_and_removes_every_marker() {
    let source = json!({
        "templates": {"base": {"enabled": true}},
        "outer": {
            "inner": {"use": "base", "name": "inside"},
            "items": [{"use": "base"}]
        }
    });
    let expanded = expand_templates(&source).unwrap();
    assert_eq!(
        expanded,
        json!({
            "outer": {
                "inner": {"enabled": true, "name": "inside"},
                "items": [{"enabled": true}]
            }
        })
    );
    assert!(!expanded.to_string().contains("\"use\""));
    assert!(!expanded.to_string().contains("\"templates\""));
}

#[test]
fn rejects_unknown_templates_cycles_and_invalid_use_shapes() {
    let invalid = [
        json!({"item": {"use": "missing"}}),
        json!({
            "templates": {"a": {"use": "b"}, "b": {"use": "a"}},
            "item": {"use": "a"}
        }),
        json!({"templates": {"bad": 1}, "item": {"use": "bad"}}),
        json!({"templates": [], "item": {}}),
        json!({"item": {"use": 1}}),
        json!({"item": {"use": ["valid", 2]}}),
    ];
    for source in invalid {
        assert!(matches!(
            expand_templates(&source),
            Err(ConfigSourceError::Template(_))
        ));
    }
}

#[test]
fn applies_the_inheritance_depth_bound() {
    let mut templates = Map::new();
    for index in 0..=MAX_EXPANSION_DEPTH {
        let value = if index == MAX_EXPANSION_DEPTH {
            json!({"value": true})
        } else {
            json!({"use": format!("template-{}", index + 1)})
        };
        templates.insert(format!("template-{index}"), value);
    }
    let source = json!({
        "templates": templates,
        "item": {"use": "template-0"}
    });
    assert!(matches!(
        expand_templates(&source),
        Err(ConfigSourceError::ExpansionDepth)
    ));
}

#[test]
fn bounds_expanded_output_independently_of_input() {
    let chunk = "x".repeat(MAX_STRING_BYTES);
    let mut root = Map::new();
    root.insert("templates".to_owned(), json!({"large": {"chunk": chunk}}));
    for index in 0..5 {
        root.insert(format!("copy-{index}"), json!({"use": "large"}));
    }
    assert!(matches!(
        expand_templates(&Value::Object(root)),
        Err(ConfigSourceError::OutputTooLarge)
    ));
}
