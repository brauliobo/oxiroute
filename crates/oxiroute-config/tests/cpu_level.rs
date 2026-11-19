use oxiroute_config::{ConfigDraft, ConfigError};
use oxiroute_cpu::Level;
use serde_json::{Value, json};

fn validate(level: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "cpu_level": level}),
    )
    .expect("authored configuration")
    .validate()
}

#[test]
fn cpu_level_is_absent_by_default_and_stays_out_of_the_canonical_form() {
    let config: ConfigDraft =
        serde_json::from_value(json!({"version": 1, "listeners": []})).unwrap();

    assert_eq!(config.cpu_level, None);
    assert_eq!(config.cpu_level_choice(), None);
    let canonical = serde_json::to_value(config.validate().unwrap()).unwrap();
    assert!(canonical.get("cpu_level").is_none());
}

#[test]
fn cpu_level_accepts_every_level_name() {
    for level in Level::ALL {
        let config = validate(&json!(level.name())).unwrap();
        assert_eq!(config.as_draft().cpu_level_choice(), Some(level));
    }
}

#[test]
fn cpu_level_rejects_anything_but_a_level_name_naming_the_setting() {
    for level in [json!(""), json!("avx"), json!("AVX2"), json!(" avx2")] {
        let error = validate(&level).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidCpuLevel(_)),
            "{level}: {error}"
        );
        assert!(error.to_string().starts_with("cpu_level "), "{error}");
    }
}
