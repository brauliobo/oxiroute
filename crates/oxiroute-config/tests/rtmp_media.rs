use oxiroute_config::{ConfigError, render_lua, validate_config};
use serde_json::{Value, json};

fn config_with_application(media: Value) -> oxiroute_config::Config {
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
    let mut config = config_with_application(json!({
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
fn dash_is_rejected_without_a_supported_muxer() {
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

    assert!(matches!(
        validate_config(&mut config),
        Err(ConfigError::UnsupportedRtmpDash { .. })
    ));
}
