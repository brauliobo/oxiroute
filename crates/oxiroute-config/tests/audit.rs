use oxiroute_config::{Audit, ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn validate(audit: &Value) -> Result<oxiroute_config::ValidatedConfig, ConfigError> {
    serde_json::from_value::<ConfigDraft>(json!({"version": 1, "listeners": [], "audit": audit}))
        .expect("authored configuration")
        .validate()
}

fn rejected(audit: &Value) -> (String, String) {
    match validate(audit).unwrap_err() {
        ConfigError::InvalidAudit { pointer, detail } => (pointer.to_owned(), detail),
        error => panic!("unexpected error: {error}"),
    }
}

#[test]
fn audit_is_absent_by_default_and_stays_out_of_the_canonical_form() {
    let config: ConfigDraft =
        serde_json::from_value(json!({"version": 1, "listeners": []})).unwrap();
    assert_eq!(config.audit, None);
    let canonical = serde_json::to_value(config.validate().unwrap()).unwrap();
    assert!(canonical.get("audit").is_none());
}

#[test]
fn an_empty_audit_block_normalizes_to_the_defaults() {
    let config = validate(&json!({})).unwrap();
    assert_eq!(config.as_draft().audit, None);
}

#[test]
fn audit_accepts_its_bounds() {
    let config = validate(&json!({
        "dir": "/var/lib/oxiroute/audit",
        "max_records": Audit::MAX_RECORDS,
        "max_record_bytes": 1,
        "max_file_bytes": Audit::MAX_FILE_BYTES,
        "max_total_bytes": Audit::MAX_TOTAL_BYTES,
        "max_rotated_files": Audit::MAX_ROTATED_FILES,
    }))
    .unwrap();

    let audit = config.as_draft().audit.as_ref().unwrap();
    assert_eq!(
        audit.dir.as_deref(),
        Some(std::path::Path::new("/var/lib/oxiroute/audit"))
    );
    assert_eq!(audit.max_records, Some(Audit::MAX_RECORDS));
    assert_eq!(audit.max_total_bytes, Some(Audit::MAX_TOTAL_BYTES));
}

#[test]
fn audit_rejects_zero_and_oversized_limits_naming_the_setting() {
    for (key, maximum) in [
        ("max_records", Audit::MAX_RECORDS),
        ("max_record_bytes", Audit::MAX_RECORD_BYTES),
        ("max_file_bytes", Audit::MAX_FILE_BYTES),
        ("max_total_bytes", Audit::MAX_TOTAL_BYTES),
        ("max_rotated_files", Audit::MAX_ROTATED_FILES),
    ] {
        for value in [0, maximum + 1] {
            let (pointer, _) = rejected(&json!({ key: value }));
            assert_eq!(pointer, format!("/audit/{key}"), "{key}={value}");
        }
    }
}

#[test]
fn audit_rejects_limits_that_contradict_each_other_or_the_defaults() {
    let (pointer, _) = rejected(&json!({"max_record_bytes": 2_000_000}));
    assert_eq!(pointer, "/audit/max_record_bytes");
    let (pointer, _) = rejected(&json!({"max_total_bytes": 1_000}));
    assert_eq!(pointer, "/audit/max_total_bytes");
    let (pointer, _) = rejected(&json!({"max_file_bytes": 4_096, "max_record_bytes": 8_192}));
    assert_eq!(pointer, "/audit/max_record_bytes");
}

#[test]
fn audit_dir_must_be_an_absolute_directory_path() {
    for path in ["audit", "", "/var/../audit"] {
        let error = validate(&json!({"dir": path})).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidFilePath { field: "dir", .. }),
            "{path}: {error}"
        );
    }
}
