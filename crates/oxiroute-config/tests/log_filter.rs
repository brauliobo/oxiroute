use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(filter: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "log_filter": filter}),
    )
    .expect("authored configuration")
    .validate()
}

#[test]
fn log_filter_is_absent_by_default_and_stays_out_of_the_canonical_form() {
    let config: ConfigDraft =
        serde_json::from_value(json!({"version": 1, "listeners": []})).unwrap();

    assert_eq!(config.log_filter, None);
    let canonical = serde_json::to_value(config.validate().unwrap()).unwrap();
    assert!(canonical.get("log_filter").is_none());
}

#[test]
fn log_filter_accepts_levels_and_per_module_directives() {
    for filter in [
        "info",
        "OFF",
        "warn,oxiroute::audit=debug",
        "oxiroute=trace",
        "  info , oxiroute_server::http=debug ",
    ] {
        let config = validate(&json!(filter)).unwrap();
        assert_eq!(config.as_draft().log_filter.as_deref(), Some(filter.trim()));
    }
}

#[test]
fn log_filter_rejects_empty_unknown_and_regex_directives() {
    for filter in [
        "",
        "   ",
        "info,",
        "oxiroute=loud",
        "oxiroute=",
        "=info",
        "info/regex",
        "a=b=c",
        "bad target=info",
        &"x".repeat(1025),
    ] {
        let error = validate(&json!(filter)).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidLogFilter { .. }),
            "{filter:?}: {error}"
        );
    }
}
