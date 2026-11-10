use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(management: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(
        json!({"version": 1, "listeners": [], "management": management}),
    )
    .expect("authored configuration")
    .validate()
}

#[test]
fn token_file_is_absent_by_default_and_stays_out_of_the_canonical_form() {
    let config = validate(&json!({"bind": "127.0.0.1:9900"})).unwrap();

    let management = config.as_draft().management.as_ref().unwrap();
    assert_eq!(management.token_file, None);
    assert!(
        serde_json::to_value(&config).unwrap()["management"]
            .get("token_file")
            .is_none()
    );
}

#[test]
fn token_file_accepts_an_absolute_path() {
    let config = validate(
        &json!({"bind": "127.0.0.1:9900", "token_file": "/etc/oxiroute/management.token"}),
    )
    .unwrap();

    let management = config.as_draft().management.as_ref().unwrap();
    assert_eq!(
        management.token_file.as_deref(),
        Some(std::path::Path::new("/etc/oxiroute/management.token"))
    );
}

#[test]
fn token_file_rejects_relative_and_empty_paths() {
    for path in ["management.token", "", "/etc/oxiroute/../token"] {
        let error = validate(&json!({"bind": "127.0.0.1:9900", "token_file": path})).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::InvalidFilePath {
                    field: "token_file",
                    ..
                }
            ),
            "{path}: {error}"
        );
    }
}
