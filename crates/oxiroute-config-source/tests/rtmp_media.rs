mod lua_support;

use lua_support::render_lua;
use oxiroute_config::{ConfigDraft, ConfigError};
use serde_json::{Value, json};

fn config_with_application(media: &Value) -> ConfigDraft {
    serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "hls": media
            }]
        }]
    }))
    .expect("valid typed configuration")
}

#[test]
fn hls_defaults_are_bounded_and_rendered() {
    let config = config_with_application(&json!({
        "root_directory": "/var/lib/oxiroute/hls",
        "variants": [{
            "name": "main",
            "bandwidth": 1_000_000,
            "codecs": "avc1.42e01e,mp4a.40.2",
            "width": 1280,
            "height": 720
        }],
        "keys": {
            "rotation_segments": 5,
            "url_prefix": "keys/"
        }
    }));

    let config = config.validate().expect("bounded HLS configuration");
    let config = config.as_draft();
    let hls = config.rtmp_services[0].applications[0]
        .hls
        .as_ref()
        .expect("HLS policy");
    assert_eq!(hls.segment_duration_ms, 2_000);
    assert_eq!(hls.max_segment_duration_ms, 10_000);
    assert_eq!(hls.playlist_length_ms, 30_000);
    assert_eq!(hls.max_queue_messages, 256);
    assert_eq!(hls.max_storage_files, 10_000);
    let lua = render_lua(config).expect("rendered HLS configuration");
    assert!(lua.contains("hls = {"));
    assert!(lua.contains("fragment_naming = \"sequential\""));
}

#[test]
fn dash_defaults_are_bounded_and_rendered() {
    let config: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "dash": {"root_directory": "/var/lib/oxiroute/dash"}
            }]
        }]
    }))
    .expect("valid typed configuration");

    let config = config.validate().expect("bounded DASH configuration");
    let config = config.as_draft();
    let dash = config.rtmp_services[0].applications[0]
        .dash
        .as_ref()
        .expect("DASH policy");
    assert_eq!(dash.segment_duration_ms, 5_000);
    assert_eq!(dash.max_segment_duration_ms, 15_000);
    assert_eq!(dash.playlist_length_ms, 30_000);
    assert_eq!(dash.max_queue_messages, 256);
    assert_eq!(dash.max_storage_files, 10_000);
    let lua = render_lua(config).expect("rendered DASH configuration");
    assert!(lua.contains("dash = {"));
    assert!(lua.contains("segment_naming = \"sequential\""));
}

#[test]
fn dash_rejects_malformed_duration_quota_and_unknown_fields() {
    let invalid: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "dash": {
                    "root_directory": "/var/lib/oxiroute/dash",
                    "segment_duration_ms": 0
                }
            }]
        }]
    }))
    .expect("valid typed configuration");
    assert!(matches!(
        invalid.validate(),
        Err(ConfigError::InvalidRtmpApplicationPolicy {
            field: "dash.segment_duration_ms",
            ..
        })
    ));

    let quota: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "dash": {
                    "root_directory": "/var/lib/oxiroute/dash",
                    "max_segment_bytes": 1024,
                    "max_storage_bytes": 512
                }
            }]
        }]
    }))
    .expect("valid typed configuration");
    assert!(matches!(
        quota.validate(),
        Err(ConfigError::InvalidRtmpApplicationPolicy {
            field: "dash.max_storage_bytes",
            ..
        })
    ));

    let unknown = serde_json::from_value::<ConfigDraft>(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "dash": {
                    "root_directory": "/var/lib/oxiroute/dash",
                    "not_a_dash_field": true
                }
            }]
        }]
    }));
    assert!(unknown.is_err());
}

#[test]
fn auto_push_defaults_are_bounded_and_rendered() {
    let config: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "auto_push": {
                "enabled": true,
                "socket_dir": "/var/run/oxiroute/rtmp",
                "reconnect_ms": 250
            },
            "applications": [{"name": "broadcast", "live": true}]
        }]
    }))
    .expect("valid typed configuration");

    let config = config.validate().expect("bounded auto-push configuration");
    let config = config.as_draft();
    let policy = &config.rtmp_services[0].auto_push;
    assert!(policy.enabled);
    assert_eq!(policy.reconnect_ms, 250);
    assert_eq!(policy.max_peers, 8);
    assert_eq!(policy.max_queue_bytes, 8 * 1024 * 1024);
    let lua = render_lua(config).expect("rendered auto-push configuration");
    assert!(lua.contains("auto_push = {"));
    assert!(lua.contains("socket_dir = \"/var/run/oxiroute/rtmp\""));
}

#[test]
fn auto_push_rejects_unsafe_paths_and_zero_bounds() {
    let invalid: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "auto_push": {
                "enabled": true,
                "socket_dir": "relative",
                "max_peers": 0
            },
            "applications": [{"name": "broadcast", "live": true}]
        }]
    }))
    .expect("typed configuration");

    assert!(matches!(
        invalid.validate(),
        Err(ConfigError::InvalidRtmpServicePolicy {
            field: "auto_push.socket_dir",
            ..
        })
    ));
}

#[test]
fn relay_dns_refresh_is_bounded_defaulted_and_rendered() {
    let defaults = config_with_application(&Value::Null)
        .validate()
        .expect("default RTMP relay policy");
    let defaults = defaults.as_draft();
    assert_eq!(
        defaults.rtmp_services[0].applications[0]
            .relay
            .dns_refresh_ms,
        60_000
    );
    let rendered = render_lua(defaults).expect("rendered RTMP relay policy");
    assert!(rendered.contains("dns_refresh_ms = 60000"));

    let below_minimum: ConfigDraft = serde_json::from_value(json!({
        "version": 1,
        "listeners": [],
        "rtmp_services": [{
            "name": "live",
            "applications": [{
                "name": "broadcast",
                "live": true,
                "relay": {"dns_refresh_ms": 999}
            }]
        }]
    }))
    .expect("typed RTMP relay policy");
    assert!(matches!(
        below_minimum.validate(),
        Err(ConfigError::InvalidRtmpApplicationPolicy {
            field: "relay.dns_refresh_ms",
            ..
        })
    ));
}

#[test]
fn rtmp_service_message_and_acknowledgement_bounds_are_validated() {
    let mut message_invalid = config_with_application(&Value::Null);
    message_invalid.rtmp_services[0].max_inbound_message_size = 0;
    assert!(matches!(
        message_invalid.validate(),
        Err(ConfigError::InvalidRtmpServicePolicy {
            field: "max_inbound_message_size",
            ..
        })
    ));

    let mut acknowledgement_invalid = config_with_application(&Value::Null);
    acknowledgement_invalid.rtmp_services[0].ack_window_size = 0;
    assert!(matches!(
        acknowledgement_invalid.validate(),
        Err(ConfigError::InvalidRtmpServicePolicy {
            field: "ack_window_size",
            ..
        })
    ));
}
