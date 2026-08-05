use oxiroute_config::{ConfigError, render_lua, validate_config};
use serde_json::{Value, json};

fn config_with_application(media: &Value) -> oxiroute_config::Config {
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
    let mut config = config_with_application(&json!({
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

    validate_config(&mut config).expect("bounded HLS configuration");
    let hls = config.rtmp_services[0].applications[0]
        .hls
        .as_ref()
        .expect("HLS policy");
    assert_eq!(hls.segment_duration_ms, 2_000);
    assert_eq!(hls.max_segment_duration_ms, 10_000);
    assert_eq!(hls.playlist_length_ms, 30_000);
    assert_eq!(hls.max_queue_messages, 256);
    assert_eq!(hls.max_storage_files, 10_000);
    let lua = render_lua(&config).expect("rendered HLS configuration");
    assert!(lua.contains("hls = {"));
    assert!(lua.contains("fragment_naming = \"sequential\""));
}

#[test]
fn dash_defaults_are_bounded_and_rendered() {
    let mut config = serde_json::from_value(json!({
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

    validate_config(&mut config).expect("bounded DASH configuration");
    let dash = config.rtmp_services[0].applications[0]
        .dash
        .as_ref()
        .expect("DASH policy");
    assert_eq!(dash.segment_duration_ms, 5_000);
    assert_eq!(dash.max_segment_duration_ms, 15_000);
    assert_eq!(dash.playlist_length_ms, 30_000);
    assert_eq!(dash.max_queue_messages, 256);
    assert_eq!(dash.max_storage_files, 10_000);
    let lua = render_lua(&config).expect("rendered DASH configuration");
    assert!(lua.contains("dash = {"));
    assert!(lua.contains("segment_naming = \"sequential\""));
}

#[test]
fn dash_rejects_malformed_duration_quota_and_unknown_fields() {
    let mut invalid = serde_json::from_value(json!({
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
        validate_config(&mut invalid),
        Err(ConfigError::InvalidRtmpApplicationPolicy {
            field: "dash.segment_duration_ms",
            ..
        })
    ));

    let mut quota = serde_json::from_value(json!({
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
        validate_config(&mut quota),
        Err(ConfigError::InvalidRtmpApplicationPolicy {
            field: "dash.max_storage_bytes",
            ..
        })
    ));

    let unknown = serde_json::from_value::<oxiroute_config::Config>(json!({
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
    let mut config: oxiroute_config::Config = serde_json::from_value(json!({
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

    validate_config(&mut config).expect("bounded auto-push configuration");
    let policy = &config.rtmp_services[0].auto_push;
    assert!(policy.enabled);
    assert_eq!(policy.reconnect_ms, 250);
    assert_eq!(policy.max_peers, 8);
    assert_eq!(policy.max_queue_bytes, 8 * 1024 * 1024);
    let lua = render_lua(&config).expect("rendered auto-push configuration");
    assert!(lua.contains("auto_push = {"));
    assert!(lua.contains("socket_dir = \"/var/run/oxiroute/rtmp\""));
}

#[test]
fn auto_push_rejects_unsafe_paths_and_zero_bounds() {
    let mut invalid: oxiroute_config::Config = serde_json::from_value(json!({
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
        validate_config(&mut invalid),
        Err(ConfigError::InvalidRtmpServicePolicy {
            field: "auto_push.socket_dir",
            ..
        })
    ));
}
