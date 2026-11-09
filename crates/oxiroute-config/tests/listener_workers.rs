use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(extra: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
}

#[test]
fn listener_workers_are_absent_by_default_and_stay_out_of_the_canonical_form() {
    let config = validate(&json!({})).unwrap();

    assert_eq!(config.as_draft().listener_workers, None);
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("listener_workers")
            .is_none()
    );
}

#[test]
fn listener_workers_accept_their_bounds() {
    for workers in [1, 4, ConfigDraft::MAX_LISTENER_WORKERS] {
        let config = validate(&json!({"listener_workers": workers})).unwrap();
        assert_eq!(config.as_draft().listener_workers, Some(workers));
    }
}

#[test]
fn listener_workers_reject_zero_and_oversized_counts() {
    for workers in [0, ConfigDraft::MAX_LISTENER_WORKERS + 1] {
        let error = validate(&json!({"listener_workers": workers})).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidListenerWorkers(value) if value == workers),
            "{error}"
        );
    }
}
