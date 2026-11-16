use oxiroute_config::ConfigDraft;
use serde_json::{Value, json};

fn draft(extra: &Value) -> Result<ConfigDraft, serde_json::Error> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(config)
}

#[test]
fn absence_turns_the_lane_on_and_stays_out_of_the_canonical_form() {
    let config = draft(&json!({})).unwrap().validate().unwrap();

    assert_eq!(config.as_draft().http_fast_lane, None);
    assert!(config.as_draft().fast_lane_enabled());
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("http_fast_lane")
            .is_none()
    );
}

#[test]
fn a_boolean_is_taken_as_written() {
    for enabled in [true, false] {
        let config = draft(&json!({"http_fast_lane": enabled}))
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(config.as_draft().http_fast_lane, Some(enabled));
        assert_eq!(config.as_draft().fast_lane_enabled(), enabled);
    }
}

#[test]
fn anything_but_a_boolean_is_rejected() {
    for value in [json!("off"), json!(0), json!([false])] {
        assert!(draft(&json!({"http_fast_lane": value})).is_err(), "{value}");
    }
}
