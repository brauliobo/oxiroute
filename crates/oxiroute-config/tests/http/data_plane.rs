use oxiroute_config::{ConfigDraft, HttpDataPlane};
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
fn absence_leaves_the_choice_open_and_stays_out_of_the_canonical_form() {
    let config = draft(&json!({})).unwrap().validate().unwrap();

    assert_eq!(config.as_draft().http_data_plane_choice(), None);
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("http_data_plane")
            .is_none()
    );
}

#[test]
fn both_planes_are_taken_as_written_and_round_trip() {
    for (token, plane) in [
        ("epoll", HttpDataPlane::Epoll),
        ("tokio", HttpDataPlane::Tokio),
    ] {
        let config = draft(&json!({"http_data_plane": token}))
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(config.as_draft().http_data_plane_choice(), Some(plane));
        assert_eq!(plane.as_str(), token);
        assert_eq!(
            serde_json::to_value(&config).unwrap()["http_data_plane"],
            token
        );
    }
}

#[test]
fn anything_but_a_plane_name_is_rejected() {
    for value in [json!("io_uring"), json!(true), json!(0), json!(["epoll"])] {
        assert!(
            draft(&json!({"http_data_plane": value})).is_err(),
            "{value}"
        );
    }
}
