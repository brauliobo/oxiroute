use oxiroute_config::{ConfigDraft, CongestionControl, Http3Transport};
use serde_json::{Value, json};

fn draft(extra: &Value) -> serde_json::Result<ConfigDraft> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(config)
}

#[test]
fn absence_is_bbr_with_segmentation_offload_and_stays_out_of_the_canonical_form() {
    let config = draft(&json!({})).unwrap().validate().unwrap();

    assert_eq!(
        config.as_draft().effective_http3_transport(),
        Http3Transport {
            congestion_control: CongestionControl::Bbr,
            segmentation_offload: true
        }
    );
    let canonical = serde_json::to_value(&config).unwrap();
    assert!(canonical.get("http3_congestion_control").is_none());
    assert!(canonical.get("http3_segmentation_offload").is_none());
}

#[test]
fn both_options_are_taken_as_written() {
    let config = draft(&json!({
        "http3_congestion_control": "cubic",
        "http3_segmentation_offload": false
    }))
    .unwrap()
    .validate()
    .unwrap();

    assert_eq!(
        config.as_draft().effective_http3_transport(),
        Http3Transport {
            congestion_control: CongestionControl::Cubic,
            segmentation_offload: false
        }
    );
    let canonical = serde_json::to_value(&config).unwrap();
    assert_eq!(canonical["http3_congestion_control"], "cubic");
    assert_eq!(canonical["http3_segmentation_offload"], false);
}

#[test]
fn an_unknown_congestion_controller_is_refused() {
    let error = draft(&json!({"http3_congestion_control": "reno"})).unwrap_err();
    assert!(error.to_string().contains("reno"), "{error}");
}
