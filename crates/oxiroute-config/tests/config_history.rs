use oxiroute_config::{ConfigDraft, ConfigError, ConfigHistory};
use serde_json::{Value, json};

fn validate(history: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "config_history": history}),
    )
    .expect("authored configuration")
    .validate()
}

fn rejected(history: &Value) -> (String, String) {
    match validate(history).unwrap_err() {
        ConfigError::InvalidConfigHistory { pointer, detail } => (pointer.to_owned(), detail),
        error => panic!("unexpected error: {error}"),
    }
}

#[test]
fn the_history_bounds_are_absent_by_default_and_stay_out_of_the_canonical_form() {
    let config: ConfigDraft =
        serde_json::from_value(json!({"version": 1, "listeners": []})).unwrap();
    assert_eq!(config.config_history, None);
    let canonical = serde_json::to_value(config.validate().unwrap()).unwrap();
    assert!(canonical.get("config_history").is_none());
}

#[test]
fn an_empty_block_normalizes_to_the_defaults() {
    let config = validate(&json!({})).unwrap();
    assert_eq!(config.as_draft().config_history, None);
}

#[test]
fn the_bounds_accept_their_limits() {
    for (entries, bytes) in [
        (1, ConfigHistory::MIN_TOTAL_BYTES),
        (ConfigHistory::MAX_ENTRIES, ConfigHistory::MAX_TOTAL_BYTES),
    ] {
        let config = validate(&json!({"max_entries": entries, "max_total_bytes": bytes})).unwrap();
        let history = config.as_draft().config_history.clone().unwrap();
        assert_eq!(history.max_entries, Some(entries));
        assert_eq!(history.max_total_bytes, Some(bytes));
    }
}

#[test]
fn out_of_range_bounds_name_the_setting_and_the_range() {
    for (history, pointer, range) in [
        (
            json!({"max_entries": 0}),
            "/config_history/max_entries",
            "between 1 and 1000",
        ),
        (
            json!({"max_entries": 1001}),
            "/config_history/max_entries",
            "between 1 and 1000",
        ),
        (
            json!({"max_total_bytes": 1024}),
            "/config_history/max_total_bytes",
            "between 4194304 and 1073741824",
        ),
        (
            json!({"max_total_bytes": 2_147_483_648_u64}),
            "/config_history/max_total_bytes",
            "between 4194304 and 1073741824",
        ),
    ] {
        let (found, detail) = rejected(&history);
        assert_eq!(found, pointer);
        assert!(detail.contains(range), "{detail}");
    }
}

#[test]
fn unknown_settings_are_refused() {
    let result = serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "config_history": {"max_files": 3}}),
    );
    assert!(result.is_err());
}

#[test]
fn the_error_points_at_the_setting() {
    let error = validate(&json!({"max_entries": 0})).unwrap_err();
    assert_eq!(
        error.pointer().as_deref(),
        Some("/config_history/max_entries")
    );
}
