use std::path::Path;

use oxiroute_config::{ConfigDraft, ValidatedConfig};
use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use serde_json::json;

const FORMATS: [ConfigFormat; 4] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
];

fn validated(extra: &serde_json::Value) -> ValidatedConfig {
    let mut config = json!({"version": 1, "listeners": []});
    config
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value::<ConfigDraft>(config)
        .expect("authored configuration")
        .validate()
        .expect("valid configuration")
}

fn assert_round_trips(config: &ValidatedConfig) {
    for format in FORMATS {
        let rendered = render_config(format, config).expect("render");
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|error| panic!("{format:?}: {error}\n{rendered}"));
        assert_eq!(&resolved.config, config, "{format:?}\n{rendered}");
    }
}

#[test]
fn management_token_file_round_trips_through_every_source_format() {
    assert_round_trips(&validated(&json!({"management": {
        "bind": "127.0.0.1:9900",
        "token_file": "/etc/oxiroute/management.token",
    }})));
}

#[test]
fn audit_settings_round_trip_through_every_source_format() {
    assert_round_trips(&validated(&json!({"audit": {
        "dir": "/var/lib/oxiroute/audit",
        "max_records": 5_000,
        "max_record_bytes": 8_192,
        "max_file_bytes": 524_288,
        "max_total_bytes": 4_194_304,
        "max_rotated_files": 3,
    }})));
}

#[test]
fn config_history_bounds_round_trip_through_every_source_format() {
    assert_round_trips(&validated(&json!({"config_history": {
        "max_entries": 50,
        "max_total_bytes": 33_554_432,
    }})));
    assert_round_trips(&validated(&json!({"config_history": {"max_entries": 5}})));
}

#[test]
fn client_tracking_settings_round_trip_through_every_source_format() {
    assert_round_trips(&validated(&json!({"client_tracking": {
        "enabled": false,
        "max_clients": 250,
        "ipv6_prefix": 64,
    }})));
    assert_round_trips(&validated(
        &json!({"client_tracking": {"ipv6_prefix": 128}}),
    ));
    assert_round_trips(&validated(&json!({"client_tracking": {"max_clients": 1}})));
}

#[test]
fn an_out_of_range_client_tracking_value_names_its_pointer_in_every_source_format() {
    let config = validated(&json!({"client_tracking": {"max_clients": 777_777}}));
    for format in FORMATS {
        let rendered = render_config(format, &config).expect("render");
        let tampered = rendered.replace("777777", "2000000");
        let error = resolve_source_with_format(Path::new("config"), tampered.as_bytes(), format)
            .expect_err("out-of-range max_clients");
        assert!(
            error.to_string().contains("/client_tracking/max_clients"),
            "{format:?}: {error}"
        );
    }
}

#[test]
fn a_partial_audit_block_round_trips_through_every_source_format() {
    assert_round_trips(&validated(&json!({"audit": {"max_records": 500}})));
}

#[test]
fn log_filter_round_trips_through_every_source_format() {
    assert_round_trips(&validated(
        &json!({"log_filter": "warn,oxiroute::audit=debug"}),
    ));
}

#[test]
fn cpu_level_round_trips_through_every_source_format() {
    assert_round_trips(&validated(&json!({"cpu_level": "sse42"})));
}
