use std::collections::HashSet;

use oxiroute_rtmp::{
    directive_specs, validate_directive, DirectiveContext, DirectiveError, RelayKind,
    RuntimeSupport, ValueKind,
};

const EXPECTED_DIRECTIVES: [&str; 117] = [
    "access_log",
    "ack_window",
    "allow",
    "application",
    "buffer",
    "buflen",
    "busy",
    "chunk_size",
    "dash",
    "dash_cleanup",
    "dash_fragment",
    "dash_nested",
    "dash_path",
    "dash_playlist_length",
    "deny",
    "drop_idle_publisher",
    "exec",
    "exec_kill_signal",
    "exec_options",
    "exec_play",
    "exec_play_done",
    "exec_publish",
    "exec_publish_done",
    "exec_pull",
    "exec_push",
    "exec_record_done",
    "exec_static",
    "hls",
    "hls_audio_buffer_size",
    "hls_base_url",
    "hls_cleanup",
    "hls_continuous",
    "hls_fragment",
    "hls_fragment_naming",
    "hls_fragment_naming_granularity",
    "hls_fragment_slicing",
    "hls_fragments_per_key",
    "hls_key_path",
    "hls_key_url",
    "hls_keys",
    "hls_max_audio_delay",
    "hls_max_fragment",
    "hls_muxdelay",
    "hls_nested",
    "hls_path",
    "hls_playlist_length",
    "hls_sync",
    "hls_type",
    "hls_variant",
    "idle_streams",
    "interleave",
    "listen",
    "live",
    "log_format",
    "max_connections",
    "max_message",
    "max_streams",
    "meta",
    "netcall_buffer",
    "netcall_timeout",
    "notify_method",
    "notify_relay_redirect",
    "notify_update_strict",
    "notify_update_timeout",
    "on_connect",
    "on_disconnect",
    "on_done",
    "on_play",
    "on_play_done",
    "on_publish",
    "on_publish_done",
    "on_record_done",
    "on_update",
    "out_cork",
    "out_queue",
    "ping",
    "ping_timeout",
    "play",
    "play_local_path",
    "play_restart",
    "play_temp_path",
    "play_time_fix",
    "publish_notify",
    "publish_time_fix",
    "pull",
    "pull_reconnect",
    "push",
    "push_reconnect",
    "record",
    "record_append",
    "record_interval",
    "record_lock",
    "record_max_frames",
    "record_max_size",
    "record_notify",
    "record_path",
    "record_suffix",
    "record_unique",
    "recorder",
    "relay_buffer",
    "respawn",
    "respawn_timeout",
    "rtmp",
    "rtmp_auto_push",
    "rtmp_auto_push_reconnect",
    "rtmp_control",
    "rtmp_socket_dir",
    "rtmp_stat",
    "rtmp_stat_stylesheet",
    "server",
    "session_relay",
    "so_keepalive",
    "stream_buckets",
    "sync",
    "timeout",
    "wait_key",
    "wait_video",
];

#[test]
fn registers_the_complete_nginx_rtmp_directive_surface() {
    let specs = directive_specs();
    let actual: HashSet<_> = specs.iter().map(|spec| spec.name).collect();
    let expected: HashSet<_> = EXPECTED_DIRECTIVES.into_iter().collect();

    assert_eq!(specs.len(), 117);
    assert_eq!(actual.len(), 117, "directive names must be unique");
    assert_eq!(actual, expected);
}

#[test]
fn records_reference_quirks_without_claiming_runtime_enforcement() {
    let stream_buckets = spec("stream_buckets");
    let hls_muxdelay = spec("hls_muxdelay");
    let so_keepalive = spec("so_keepalive");
    let push = spec("push");

    assert_eq!(stream_buckets.runtime_support, RuntimeSupport::SourceBug);
    assert_eq!(hls_muxdelay.runtime_support, RuntimeSupport::SourceNoOp);
    assert_eq!(so_keepalive.runtime_support, RuntimeSupport::Deprecated);
    assert_eq!(push.value_kind, ValueKind::RelayTarget(RelayKind::Push));
}

#[test]
fn validates_contexts_arities_and_closed_value_sets() {
    assert!(validate_directive("meta", DirectiveContext::RtmpApplication, &["copy"]).is_ok());
    assert!(validate_directive(
        "record",
        DirectiveContext::RtmpRecorder,
        &["audio", "video"]
    )
    .is_ok());
    assert!(validate_directive(
        "hls_fragment_naming",
        DirectiveContext::RtmpApplication,
        &["timestamp"]
    )
    .is_ok());
    assert!(
        validate_directive("rtmp_control", DirectiveContext::Http, &["record", "drop"]).is_ok()
    );
    assert!(validate_directive("exec_kill_signal", DirectiveContext::RtmpMain, &["TERM"]).is_ok());
    assert!(validate_directive(
        "listen",
        DirectiveContext::RtmpServer,
        &["1935", "proxy_protocol"]
    )
    .is_ok());
    assert!(validate_directive(
        "pull",
        DirectiveContext::RtmpApplication,
        &["rtmp://origin/live", "name=cam", "static"]
    )
    .is_ok());

    assert!(matches!(
        validate_directive("hls", DirectiveContext::NginxMain, &["on"]),
        Err(DirectiveError::InvalidContext { .. })
    ));
    assert!(matches!(
        validate_directive("meta", DirectiveContext::RtmpApplication, &["passthrough"]),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive(
            "listen",
            DirectiveContext::RtmpServer,
            &["1935", "bind", "proxy_protocol"]
        ),
        Err(DirectiveError::InvalidArity { .. })
    ));
    assert!(matches!(
        validate_directive(
            "push",
            DirectiveContext::RtmpApplication,
            &["origin", "static"]
        ),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive(
            "pull",
            DirectiveContext::RtmpApplication,
            &["origin", "static"]
        ),
        Err(DirectiveError::InvalidValue { .. })
    ));
}

fn spec(name: &str) -> &'static oxiroute_rtmp::DirectiveSpec {
    directive_specs()
        .iter()
        .find(|spec| spec.name == name)
        .expect("registered directive")
}
