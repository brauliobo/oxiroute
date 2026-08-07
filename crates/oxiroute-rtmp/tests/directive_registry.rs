use std::collections::HashSet;

use oxiroute_rtmp::{
    DirectiveContext, DirectiveError, DirectiveStatus, RelayKind, RuntimeSupport, ValueKind,
    directive_compatibility_report, directive_specs, validate_directive,
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
fn reports_truthful_key_and_form_statuses() {
    let report = directive_compatibility_report();
    let explicit_form_count = report
        .entries
        .iter()
        .map(|spec| spec.runtime_forms.len())
        .sum::<usize>();

    assert_eq!(report.entries.len(), 117);
    assert_eq!(report.directive_status.total(), 117);
    assert_eq!(report.form_status.total(), explicit_form_count);
    assert_eq!(report.directive_status.enforced, 7);
    assert!(report.directive_status.enforced < report.entries.len());
    assert!(report.directive_status.partial > 0);
    assert!(report.form_status.enforced > 0);
    assert!(report.form_status.disable_only > 0);
    assert!(report.form_status.parsed_only > 0);
    assert!(report.form_status.source_no_op > 0);
    assert!(report.form_status.source_bug > 0);
    assert!(report.form_status.deprecated > 0);
    assert!(report.form_status.platform_limited > 0);
    for spec in report.entries {
        for form in spec.runtime_forms {
            assert_ne!(form.status, DirectiveStatus::Partial);
            assert!(
                form.contexts
                    .iter()
                    .all(|context| spec.contexts.contains(context)),
                "{} form {} escapes the directive context set",
                spec.name,
                form.form
            );
        }
    }
}

#[test]
fn reports_enforced_live_push_recording_chunk_and_listener_forms() {
    assert_form_status("live", "live on", DirectiveStatus::Enforced);
    assert_form_status("live", "live off", DirectiveStatus::Enforced);
    assert_form_status(
        "push",
        "one canonical static target token",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record",
        "record all with live on and secure path",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record",
        "record all manual or manual all with live on and secure path",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_path",
        "secure absolute recording root when recording is enabled",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_suffix",
        "bounded literal suffix on a canonical recorder",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "chunk_size",
        "bounded outbound chunk size (1..=1048576)",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "ack_window",
        "bounded nonzero acknowledgement window",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "max_message",
        "bounded assembled-message ceiling (1..=8388608)",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "listen",
        "numeric socket address without options",
        DirectiveStatus::Enforced,
    );
}

#[test]
fn reports_unsupported_forms_without_promoting_their_directives() {
    assert_form_status(
        "push",
        "target token with nginx relay options",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status("record", "record off", DirectiveStatus::DisableOnly);
    assert_form_status(
        "record",
        "other record bitmask",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status(
        "record_append",
        "record_append off",
        DirectiveStatus::DisableOnly,
    );
    assert_form_status(
        "record_append",
        "record_append on",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "chunk_size",
        "out-of-range outbound chunk size",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status(
        "ack_window",
        "zero acknowledgement window",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status(
        "max_message",
        "out-of-range assembled-message ceiling",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status(
        "listen",
        "socket address with a listen option",
        DirectiveStatus::ParsedOnly,
    );

    assert_eq!(
        spec("push").compatibility_status(),
        DirectiveStatus::Partial
    );
    assert_eq!(
        spec("record").compatibility_status(),
        DirectiveStatus::Partial
    );
    assert_eq!(
        spec("chunk_size").compatibility_status(),
        DirectiveStatus::Partial
    );
    assert_eq!(
        spec("listen").compatibility_status(),
        DirectiveStatus::Partial
    );
}

#[test]
fn reports_bounded_hls_exec_respawn_and_recorder_forms() {
    assert_form_status(
        "hls",
        "bounded HLS policy option in the importer-supported subset",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "hls_fragment_naming",
        "bounded HLS policy option in the importer-supported subset",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "exec_push",
        "one absolute executable plus bounded literal argv",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "exec_push",
        "redirection, control, or non-absolute executable form",
        DirectiveStatus::ParsedOnly,
    );
    assert_form_status(
        "respawn_timeout",
        "bounded respawn flag or delay applied to a typed exec profile",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record",
        "record audio, video, or keyframes with live on and secure path",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_max_size",
        "nonzero record_max_size",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_max_frames",
        "nonzero record_max_frames",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_notify",
        "record_notify on",
        DirectiveStatus::Enforced,
    );
    assert_form_status(
        "record_lock",
        "record_lock on",
        DirectiveStatus::PlatformLimited,
    );
    assert_form_status(
        "access_log",
        "absolute access-log path with optional combined format at rtmp scope",
        DirectiveStatus::Enforced,
    );

    for name in [
        "hls",
        "exec",
        "respawn",
        "record_max_size",
        "record_max_frames",
    ] {
        assert_eq!(
            spec(name).compatibility_status(),
            DirectiveStatus::Partial,
            "{name} must retain a bounded rather than broad parity claim"
        );
    }
    assert_eq!(
        spec("pull").compatibility_status(),
        DirectiveStatus::ParsedOnly
    );
    assert_eq!(
        spec("rtmp_auto_push").compatibility_status(),
        DirectiveStatus::PlatformLimited
    );
}

#[test]
fn classifies_non_enforced_source_and_platform_forms() {
    assert_eq!(
        spec("stream_buckets").compatibility_status(),
        DirectiveStatus::SourceBug
    );
    assert_eq!(
        spec("hls_muxdelay").compatibility_status(),
        DirectiveStatus::SourceNoOp
    );
    assert_eq!(
        spec("so_keepalive").compatibility_status(),
        DirectiveStatus::Deprecated
    );
    assert_eq!(
        spec("rtmp_auto_push").compatibility_status(),
        DirectiveStatus::PlatformLimited
    );
    assert_eq!(
        spec("max_streams").compatibility_status(),
        DirectiveStatus::ParsedOnly
    );
}

#[test]
fn validates_live_push_recording_chunk_and_listener_forms() {
    assert!(validate_directive("live", DirectiveContext::RtmpApplication, &["on"]).is_ok());
    assert!(
        validate_directive(
            "push",
            DirectiveContext::RtmpApplication,
            &["rtmp://origin/live"]
        )
        .is_ok()
    );
    assert!(validate_directive("record", DirectiveContext::RtmpApplication, &["all"]).is_ok());
    assert!(
        validate_directive(
            "record",
            DirectiveContext::RtmpApplication,
            &["all", "manual"]
        )
        .is_ok()
    );
    assert!(validate_directive("chunk_size", DirectiveContext::RtmpServer, &["1048576"]).is_ok());
    assert!(validate_directive("listen", DirectiveContext::RtmpServer, &["1935"]).is_ok());

    assert!(matches!(
        validate_directive("live", DirectiveContext::RtmpApplication, &["maybe"]),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive(
            "push",
            DirectiveContext::RtmpApplication,
            &["rtmp://origin/live", "unexpected=1"]
        ),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive(
            "record",
            DirectiveContext::RtmpApplication,
            &["all", "unexpected"]
        ),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive(
            "chunk_size",
            DirectiveContext::RtmpServer,
            &["not-a-number"]
        ),
        Err(DirectiveError::InvalidValue { .. })
    ));
    assert!(matches!(
        validate_directive("listen", DirectiveContext::RtmpServer, &[""]),
        Err(DirectiveError::InvalidValue { .. })
    ));
}

#[test]
fn unsupported_runtime_families_remain_explicitly_parsed_not_enforced() {
    for name in [
        "allow",
        "deny",
        "push",
        "pull",
        "play",
        "hls",
        "dash",
        "exec",
        "record_max_size",
        "record_max_frames",
        "record_notify",
    ] {
        assert_eq!(
            spec(name).runtime_support,
            RuntimeSupport::ParsedNotEnforced,
            "{name} must not claim disconnected runtime behavior"
        );
    }

    for name in [
        "live",
        "idle_streams",
        "record",
        "record_path",
        "record_suffix",
    ] {
        assert_eq!(
            spec(name).runtime_support,
            RuntimeSupport::ParsedNotEnforced,
            "native runtime equivalents do not imply nginx directive lowering"
        );
    }
}

#[test]
fn validates_contexts_arities_and_closed_value_sets() {
    assert!(validate_directive("meta", DirectiveContext::RtmpApplication, &["copy"]).is_ok());
    assert!(
        validate_directive(
            "record",
            DirectiveContext::RtmpRecorder,
            &["audio", "video"]
        )
        .is_ok()
    );
    assert!(
        validate_directive(
            "hls_fragment_naming",
            DirectiveContext::RtmpApplication,
            &["timestamp"]
        )
        .is_ok()
    );
    assert!(
        validate_directive("rtmp_control", DirectiveContext::Http, &["record", "drop"]).is_ok()
    );
    assert!(validate_directive("exec_kill_signal", DirectiveContext::RtmpMain, &["TERM"]).is_ok());
    assert!(
        validate_directive(
            "listen",
            DirectiveContext::RtmpServer,
            &["1935", "proxy_protocol"]
        )
        .is_ok()
    );
    assert!(
        validate_directive(
            "pull",
            DirectiveContext::RtmpApplication,
            &["rtmp://origin/live", "name=cam", "static"]
        )
        .is_ok()
    );

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

fn assert_form_status(name: &str, form: &str, expected: DirectiveStatus) {
    assert_eq!(
        spec(name)
            .runtime_form(form)
            .unwrap_or_else(|| panic!("missing runtime form {name}: {form}"))
            .status,
        expected,
        "unexpected status for {name} form {form}"
    );
}
