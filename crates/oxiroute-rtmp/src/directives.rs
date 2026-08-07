use std::net::IpAddr;

use crate::{
    DirectiveCompatibilityReport, DirectiveContext, DirectiveError, DirectiveForm, DirectiveSpec,
    DirectiveStatus, DirectiveStatusCounts, RelayKind, RuntimeSupport, ValueKind,
};

use DirectiveContext::{Http, NginxMain, RtmpApplication, RtmpMain, RtmpRecorder, RtmpServer};
use DirectiveStatus::{
    Deprecated as StatusDeprecated, DisableOnly, Enforced, ParsedOnly,
    PlatformLimited as StatusPlatformLimited, SourceBug as StatusSourceBug,
    SourceNoOp as StatusSourceNoOp,
};
use RuntimeSupport::{Deprecated, ParsedNotEnforced, PlatformLimited, SourceBug, SourceNoOp};

const N: &[DirectiveContext] = &[NginxMain];
const R: &[DirectiveContext] = &[RtmpMain];
const S: &[DirectiveContext] = &[RtmpServer];
const A: &[DirectiveContext] = &[RtmpApplication];
const RS: &[DirectiveContext] = &[RtmpMain, RtmpServer];
const RSA: &[DirectiveContext] = &[RtmpMain, RtmpServer, RtmpApplication];
const RSAC: &[DirectiveContext] = &[RtmpMain, RtmpServer, RtmpApplication, RtmpRecorder];
const H: &[DirectiveContext] = &[Http];

const META: &[&str] = &["off", "on", "copy"];
const NOTIFY_METHODS: &[&str] = &["get", "post"];
const RECORD_PARTS: &[&str] = &["off", "all", "audio", "video", "keyframes", "manual"];
const HLS_NAMING: &[&str] = &["sequential", "timestamp", "system"];
const HLS_SLICING: &[&str] = &["plain", "aligned"];
const HLS_TYPES: &[&str] = &["live", "event"];
const STAT_PARTS: &[&str] = &["all", "global", "live", "clients"];
const CONTROL_PARTS: &[&str] = &["all", "record", "drop", "redirect"];

const RTMP_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "canonical rtmp block",
    contexts: N,
    status: Enforced,
}];
const SERVER_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "canonical server block",
    contexts: R,
    status: Enforced,
}];
const LISTENER_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "numeric socket address without options",
        contexts: S,
        status: Enforced,
    },
    DirectiveForm {
        form: "socket address with a listen option",
        contexts: S,
        status: ParsedOnly,
    },
];
const APPLICATION_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "canonical named application block",
        contexts: S,
        status: Enforced,
    },
    DirectiveForm {
        form: "invalid or unsupported application block",
        contexts: S,
        status: ParsedOnly,
    },
];
const CHUNK_SIZE_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded outbound chunk size (1..=1048576)",
        contexts: RS,
        status: Enforced,
    },
    DirectiveForm {
        form: "out-of-range outbound chunk size",
        contexts: RS,
        status: ParsedOnly,
    },
];
const ACK_WINDOW_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded nonzero acknowledgement window",
        contexts: RS,
        status: Enforced,
    },
    DirectiveForm {
        form: "zero acknowledgement window",
        contexts: RS,
        status: ParsedOnly,
    },
];
const MAX_MESSAGE_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded assembled-message ceiling (1..=8388608)",
        contexts: RS,
        status: Enforced,
    },
    DirectiveForm {
        form: "out-of-range assembled-message ceiling",
        contexts: RS,
        status: ParsedOnly,
    },
];
const LIVE_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "live on",
        contexts: RSA,
        status: Enforced,
    },
    DirectiveForm {
        form: "live off",
        contexts: RSA,
        status: Enforced,
    },
];
const IDLE_STREAMS_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "idle_streams on",
        contexts: RSA,
        status: Enforced,
    },
    DirectiveForm {
        form: "idle_streams off",
        contexts: RSA,
        status: Enforced,
    },
];
const PUSH_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "one canonical static target token",
        contexts: A,
        status: Enforced,
    },
    DirectiveForm {
        form: "target token with nginx relay options",
        contexts: A,
        status: ParsedOnly,
    },
];
const PULL_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "pull target",
    contexts: A,
    status: ParsedOnly,
}];
const EXEC_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "one absolute executable plus bounded literal argv",
        contexts: RSA,
        status: Enforced,
    },
    DirectiveForm {
        form: "redirection, control, or non-absolute executable form",
        contexts: RSA,
        status: ParsedOnly,
    },
];
const RESPAWN_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded respawn flag or delay applied to a typed exec profile",
        contexts: RSA,
        status: Enforced,
    },
    DirectiveForm {
        form: "out-of-range respawn policy",
        contexts: RSA,
        status: ParsedOnly,
    },
];
const RECORD_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record off",
        contexts: RSAC,
        status: DisableOnly,
    },
    DirectiveForm {
        form: "record all with live on and secure path",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "record all manual or manual all with live on and secure path",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "record audio, video, or keyframes with live on and secure path",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "other record bitmask",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_PATH_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "secure absolute recording root when recording is enabled",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "other registered recording path",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_SUFFIX_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded literal suffix on a canonical recorder",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "strftime or unsafe suffix",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_UNIQUE_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_unique on for a canonical recorder",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "record_unique off for a canonical recorder",
        contexts: RSAC,
        status: Enforced,
    },
];
const RECORD_INTERVAL_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded interval on continuous recording",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "zero, overflow, or manual-recording interval",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_APPEND_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_append off",
        contexts: RSAC,
        status: DisableOnly,
    },
    DirectiveForm {
        form: "record_append on",
        contexts: RSAC,
        status: Enforced,
    },
];
const RECORD_LOCK_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_lock off",
        contexts: RSAC,
        status: DisableOnly,
    },
    DirectiveForm {
        form: "record_lock on",
        contexts: RSAC,
        status: StatusPlatformLimited,
    },
];
const RECORD_MAX_SIZE_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_max_size 0 on a canonical recorder",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "nonzero record_max_size",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "out-of-range record_max_size",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_MAX_FRAMES_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_max_frames 0 on a canonical recorder",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "nonzero record_max_frames",
        contexts: RSAC,
        status: Enforced,
    },
    DirectiveForm {
        form: "out-of-range record_max_frames",
        contexts: RSAC,
        status: ParsedOnly,
    },
];
const RECORD_NOTIFY_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "record_notify off",
        contexts: RSAC,
        status: DisableOnly,
    },
    DirectiveForm {
        form: "record_notify on",
        contexts: RSAC,
        status: Enforced,
    },
];
const RECORDER_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "canonical named recorder block",
        contexts: A,
        status: Enforced,
    },
    DirectiveForm {
        form: "invalid or unsupported recorder block",
        contexts: A,
        status: ParsedOnly,
    },
];
const ACCESS_LOG_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "access_log off at rtmp scope",
        contexts: R,
        status: DisableOnly,
    },
    DirectiveForm {
        form: "absolute access-log path with optional combined format at rtmp scope",
        contexts: R,
        status: Enforced,
    },
    DirectiveForm {
        form: "enabled or nested access logging",
        contexts: RSA,
        status: ParsedOnly,
    },
];
const ACCESS_RULE_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "bounded publish/play IP rule",
    contexts: RSA,
    status: Enforced,
}];
const MAX_CONNECTIONS_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded per-application connection ceiling",
        contexts: A,
        status: Enforced,
    },
    DirectiveForm {
        form: "service or global connection ceiling",
        contexts: RS,
        status: ParsedOnly,
    },
];
const STREAM_BUCKETS_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered stream_buckets integer",
    contexts: RSA,
    status: StatusSourceBug,
}];
const HLS_MUXDELAY_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered hls_muxdelay duration",
    contexts: RSA,
    status: StatusSourceNoOp,
}];
const HLS_FORMS: &[DirectiveForm] = &[
    DirectiveForm {
        form: "bounded HLS policy option in the importer-supported subset",
        contexts: RSA,
        status: Enforced,
    },
    DirectiveForm {
        form: "unrepresented HLS option or invalid HLS combination",
        contexts: RSA,
        status: ParsedOnly,
    },
];
const DEPRECATED_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered so_keepalive flag",
    contexts: RS,
    status: StatusDeprecated,
}];
const PLATFORM_EXEC_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered platform-owned value",
    contexts: RSA,
    status: StatusPlatformLimited,
}];
const PLATFORM_NGINX_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered platform-owned value",
    contexts: N,
    status: StatusPlatformLimited,
}];

macro_rules! directive {
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: ParsedNotEnforced,
            runtime_forms: &[],
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal, $support:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: $support,
            runtime_forms: &[],
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal; $forms:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: ParsedNotEnforced,
            runtime_forms: $forms,
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal, $support:expr; $forms:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: $support,
            runtime_forms: $forms,
        }
    };
}

static DIRECTIVES: [DirectiveSpec; 117] = [
    // Entry and core: 18
    directive!("rtmp", N, 0, Some(0), ValueKind::Block, None, false; RTMP_FORMS),
    directive!("server", R, 0, Some(0), ValueKind::Block, None, true; SERVER_FORMS),
    directive!("listen", S, 1, Some(2), ValueKind::Listen, None, true; LISTENER_FORMS),
    directive!(
        "application",
        S,
        1,
        Some(1),
        ValueKind::NamedBlock,
        None,
        true;
        APPLICATION_FORMS
    ),
    directive!(
        "so_keepalive",
        RS,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false,
        Deprecated;
        DEPRECATED_FORMS
    ),
    directive!(
        "timeout",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("60s"),
        false
    ),
    directive!(
        "ping",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("60s"),
        false
    ),
    directive!(
        "ping_timeout",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("30s"),
        false
    ),
    directive!(
        "max_streams",
        RS,
        1,
        Some(1),
        ValueKind::Integer,
        Some("32"),
        false
    ),
    directive!(
        "ack_window",
        RS,
        1,
        Some(1),
        ValueKind::Integer,
        Some("5000000"),
        false;
        ACK_WINDOW_FORMS
    ),
    directive!(
        "chunk_size",
        RS,
        1,
        Some(1),
        ValueKind::Integer,
        Some("4096"),
        false;
        CHUNK_SIZE_FORMS
    ),
    directive!(
        "max_message",
        RS,
        1,
        Some(1),
        ValueKind::Size,
        Some("1M"),
        false;
        MAX_MESSAGE_FORMS
    ),
    directive!(
        "out_queue",
        RS,
        1,
        Some(1),
        ValueKind::Size,
        Some("256"),
        false
    ),
    directive!(
        "out_cork",
        RS,
        1,
        Some(1),
        ValueKind::Size,
        Some("out_queue / 8"),
        false
    ),
    directive!("busy", RS, 1, Some(1), ValueKind::Flag, Some("off"), false),
    directive!(
        "play_time_fix",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false
    ),
    directive!(
        "publish_time_fix",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false
    ),
    directive!(
        "buflen",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("1s"),
        false
    ),
    // Access and codec: 3
    directive!(
        "allow",
        RSA,
        1,
        Some(2),
        ValueKind::AccessRule,
        Some("implicit allow"),
        true;
        ACCESS_RULE_FORMS
    ),
    directive!(
        "deny",
        RSA,
        1,
        Some(2),
        ValueKind::AccessRule,
        Some("implicit allow"),
        true;
        ACCESS_RULE_FORMS
    ),
    directive!(
        "meta",
        RSA,
        1,
        Some(1),
        ValueKind::Enum(META),
        Some("on"),
        false
    ),
    // Live: 11
    directive!("live", RSA, 1, Some(1), ValueKind::Flag, Some("off"), false; LIVE_FORMS),
    directive!(
        "stream_buckets",
        RSA,
        1,
        Some(1),
        ValueKind::Integer,
        Some("1024"),
        false,
        SourceBug;
        STREAM_BUCKETS_FORMS
    ),
    directive!(
        "buffer",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("0"),
        false
    ),
    directive!(
        "sync",
        RSA,
        1,
        Some(1),
        ValueKind::DurationOrOff,
        Some("300ms"),
        false
    ),
    directive!(
        "interleave",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    directive!(
        "wait_key",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false;
        IDLE_STREAMS_FORMS
    ),
    directive!(
        "wait_video",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    directive!(
        "publish_notify",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    directive!(
        "play_restart",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    directive!(
        "idle_streams",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false
    ),
    directive!(
        "drop_idle_publisher",
        RSA,
        1,
        Some(1),
        ValueKind::DurationOrOff,
        Some("off"),
        false
    ),
    // Relay: 6
    directive!(
        "push",
        A,
        1,
        None,
        ValueKind::RelayTarget(RelayKind::Push),
        None,
        true;
        PUSH_FORMS
    ),
    directive!(
        "pull",
        A,
        1,
        None,
        ValueKind::RelayTarget(RelayKind::Pull),
        None,
        true;
        PULL_FORMS
    ),
    directive!(
        "relay_buffer",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("5s"),
        false
    ),
    directive!(
        "push_reconnect",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("3s"),
        false
    ),
    directive!(
        "pull_reconnect",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("3s"),
        false
    ),
    directive!(
        "session_relay",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    // External execution: 13
    directive!("exec", RSA, 1, None, ValueKind::Command, None, true; EXEC_FORMS),
    directive!("exec_push", RSA, 1, None, ValueKind::Command, None, true; EXEC_FORMS),
    directive!("exec_pull", RSA, 1, None, ValueKind::Command, None, true),
    directive!("exec_publish", RSA, 1, None, ValueKind::Command, None, true; EXEC_FORMS),
    directive!(
        "exec_publish_done",
        RSA,
        1,
        None,
        ValueKind::Command,
        None,
        true;
        EXEC_FORMS
    ),
    directive!("exec_play", RSA, 1, None, ValueKind::Command, None, true),
    directive!(
        "exec_play_done",
        RSA,
        1,
        None,
        ValueKind::Command,
        None,
        true
    ),
    directive!(
        "exec_record_done",
        RSAC,
        1,
        None,
        ValueKind::Command,
        None,
        true
    ),
    directive!(
        "exec_static",
        RSA,
        1,
        None,
        ValueKind::Command,
        None,
        true,
        PlatformLimited;
        PLATFORM_EXEC_FORMS
    ),
    directive!(
        "respawn",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false;
        RESPAWN_FORMS
    ),
    directive!(
        "respawn_timeout",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("5s"),
        false;
        RESPAWN_FORMS
    ),
    directive!(
        "exec_kill_signal",
        RSA,
        1,
        Some(1),
        ValueKind::Signal,
        Some("KILL"),
        false
    ),
    directive!(
        "exec_options",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    // Recording: 11
    directive!(
        "record",
        RSAC,
        1,
        None,
        ValueKind::Bitmask(RECORD_PARTS),
        Some("off"),
        false;
        RECORD_FORMS
    ),
    directive!(
        "record_path",
        RSAC,
        1,
        Some(1),
        ValueKind::Path,
        Some(""),
        false;
        RECORD_PATH_FORMS
    ),
    directive!(
        "record_suffix",
        RSAC,
        1,
        Some(1),
        ValueKind::Strings,
        Some(".flv"),
        false;
        RECORD_SUFFIX_FORMS
    ),
    directive!(
        "record_unique",
        RSAC,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false;
        RECORD_UNIQUE_FORMS
    ),
    directive!(
        "record_append",
        RSAC,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false;
        RECORD_APPEND_FORMS
    ),
    directive!(
        "record_lock",
        RSAC,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false,
        PlatformLimited;
        RECORD_LOCK_FORMS
    ),
    directive!(
        "record_max_size",
        RSAC,
        1,
        Some(1),
        ValueKind::Size,
        Some("0"),
        false;
        RECORD_MAX_SIZE_FORMS
    ),
    directive!(
        "record_max_frames",
        RSAC,
        1,
        Some(1),
        ValueKind::Size,
        Some("0"),
        false;
        RECORD_MAX_FRAMES_FORMS
    ),
    directive!(
        "record_interval",
        RSAC,
        1,
        Some(1),
        ValueKind::Duration,
        None,
        false;
        RECORD_INTERVAL_FORMS
    ),
    directive!(
        "record_notify",
        RSAC,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false;
        RECORD_NOTIFY_FORMS
    ),
    directive!("recorder", A, 1, Some(1), ValueKind::NamedBlock, None, true; RECORDER_FORMS),
    // VOD and netcall: 5
    directive!("play", RSA, 1, None, ValueKind::Strings, None, true),
    directive!(
        "play_temp_path",
        RSA,
        1,
        Some(1),
        ValueKind::Path,
        Some("/tmp"),
        false
    ),
    directive!(
        "play_local_path",
        RSA,
        1,
        Some(1),
        ValueKind::Path,
        Some(""),
        false
    ),
    directive!(
        "netcall_timeout",
        RS,
        1,
        Some(1),
        ValueKind::Duration,
        Some("10s"),
        false
    ),
    directive!(
        "netcall_buffer",
        RS,
        1,
        Some(1),
        ValueKind::Size,
        Some("1024"),
        false
    ),
    // HTTP notifications: 13
    directive!("on_connect", RS, 1, Some(1), ValueKind::Url, None, false),
    directive!("on_disconnect", RS, 1, Some(1), ValueKind::Url, None, false),
    directive!("on_publish", RSA, 1, Some(1), ValueKind::Url, None, false),
    directive!("on_play", RSA, 1, Some(1), ValueKind::Url, None, false),
    directive!(
        "on_publish_done",
        RSA,
        1,
        Some(1),
        ValueKind::Url,
        None,
        false
    ),
    directive!("on_play_done", RSA, 1, Some(1), ValueKind::Url, None, false),
    directive!("on_done", RSA, 1, Some(1), ValueKind::Url, None, false),
    directive!(
        "on_record_done",
        RSAC,
        1,
        Some(1),
        ValueKind::Url,
        None,
        false
    ),
    directive!("on_update", RSA, 1, Some(1), ValueKind::Url, None, false),
    directive!(
        "notify_method",
        RSA,
        1,
        Some(1),
        ValueKind::Enum(NOTIFY_METHODS),
        Some("post"),
        false
    ),
    directive!(
        "notify_update_timeout",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("30s"),
        false
    ),
    directive!(
        "notify_update_strict",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    directive!(
        "notify_relay_redirect",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    // Logging, limits, and auto-push: 6
    directive!(
        "access_log",
        RSA,
        1,
        Some(2),
        ValueKind::AccessLog,
        Some("combined"),
        true;
        ACCESS_LOG_FORMS
    ),
    directive!(
        "log_format",
        RSA,
        2,
        None,
        ValueKind::LogFormat,
        Some("combined"),
        true
    ),
    directive!(
        "max_connections",
        RSA,
        1,
        Some(1),
        ValueKind::Integer,
        None,
        false;
        MAX_CONNECTIONS_FORMS
    ),
    directive!(
        "rtmp_auto_push",
        N,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false,
        PlatformLimited;
        PLATFORM_NGINX_FORMS
    ),
    directive!(
        "rtmp_auto_push_reconnect",
        N,
        1,
        Some(1),
        ValueKind::Duration,
        Some("100ms"),
        false,
        PlatformLimited;
        PLATFORM_NGINX_FORMS
    ),
    directive!(
        "rtmp_socket_dir",
        N,
        1,
        Some(1),
        ValueKind::Path,
        Some("/tmp"),
        false,
        PlatformLimited;
        PLATFORM_NGINX_FORMS
    ),
    // HLS: 22
    directive!("hls", RSA, 1, Some(1), ValueKind::Flag, Some("off"), false; HLS_FORMS),
    directive!(
        "hls_fragment",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("5s"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_max_fragment",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("10 * hls_fragment"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_path",
        RSA,
        1,
        Some(1),
        ValueKind::Path,
        Some(""),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_playlist_length",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("30s"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_muxdelay",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("700ms"),
        false,
        SourceNoOp;
        HLS_MUXDELAY_FORMS
    ),
    directive!(
        "hls_sync",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("2ms"),
        false
    ),
    directive!(
        "hls_continuous",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false
    ),
    directive!(
        "hls_nested",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_fragment_naming",
        RSA,
        1,
        Some(1),
        ValueKind::Enum(HLS_NAMING),
        Some("sequential"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_fragment_slicing",
        RSA,
        1,
        Some(1),
        ValueKind::Enum(HLS_SLICING),
        Some("plain"),
        false
    ),
    directive!(
        "hls_type",
        RSA,
        1,
        Some(1),
        ValueKind::Enum(HLS_TYPES),
        Some("live"),
        false
    ),
    directive!(
        "hls_max_audio_delay",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("300ms"),
        false
    ),
    directive!(
        "hls_audio_buffer_size",
        RSA,
        1,
        Some(1),
        ValueKind::Size,
        Some("1M"),
        false
    ),
    directive!(
        "hls_cleanup",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_variant",
        RSA,
        1,
        None,
        ValueKind::HlsVariant,
        None,
        true
    ),
    directive!(
        "hls_base_url",
        RSA,
        1,
        Some(1),
        ValueKind::Strings,
        Some(""),
        false
    ),
    directive!(
        "hls_fragment_naming_granularity",
        RSA,
        1,
        Some(1),
        ValueKind::Integer,
        Some("0"),
        false
    ),
    directive!(
        "hls_keys",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_key_path",
        RSA,
        1,
        Some(1),
        ValueKind::Path,
        Some("hls_path"),
        false
    ),
    directive!(
        "hls_key_url",
        RSA,
        1,
        Some(1),
        ValueKind::Strings,
        Some(""),
        false;
        HLS_FORMS
    ),
    directive!(
        "hls_fragments_per_key",
        RSA,
        1,
        Some(1),
        ValueKind::Integer,
        Some("0"),
        false;
        HLS_FORMS
    ),
    // DASH: 6
    directive!("dash", RSA, 1, Some(1), ValueKind::Flag, Some("off"), false),
    directive!(
        "dash_fragment",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("5s"),
        false
    ),
    directive!(
        "dash_path",
        RSA,
        1,
        Some(1),
        ValueKind::Path,
        Some(""),
        false
    ),
    directive!(
        "dash_playlist_length",
        RSA,
        1,
        Some(1),
        ValueKind::Duration,
        Some("30s"),
        false
    ),
    directive!(
        "dash_cleanup",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("on"),
        false
    ),
    directive!(
        "dash_nested",
        RSA,
        1,
        Some(1),
        ValueKind::Flag,
        Some("off"),
        false
    ),
    // HTTP statistics and control: 3
    directive!(
        "rtmp_stat",
        H,
        1,
        None,
        ValueKind::Bitmask(STAT_PARTS),
        Some("0"),
        false
    ),
    directive!(
        "rtmp_stat_stylesheet",
        H,
        1,
        Some(1),
        ValueKind::Strings,
        Some(""),
        false
    ),
    directive!(
        "rtmp_control",
        H,
        1,
        None,
        ValueKind::Bitmask(CONTROL_PARTS),
        Some("0"),
        false
    ),
];

#[must_use]
pub fn directive_specs() -> &'static [DirectiveSpec] {
    &DIRECTIVES
}

#[must_use]
pub fn directive_compatibility_report() -> DirectiveCompatibilityReport {
    let mut directive_status = DirectiveStatusCounts::default();
    let mut form_status = DirectiveStatusCounts::default();

    for spec in &DIRECTIVES {
        increment_status(&mut directive_status, spec.compatibility_status());
        for form in spec.runtime_forms {
            increment_status(&mut form_status, form.status);
        }
    }

    DirectiveCompatibilityReport {
        entries: &DIRECTIVES,
        directive_status,
        form_status,
    }
}

fn increment_status(counts: &mut DirectiveStatusCounts, status: DirectiveStatus) {
    match status {
        DirectiveStatus::Enforced => counts.enforced += 1,
        DirectiveStatus::Partial => counts.partial += 1,
        DirectiveStatus::DisableOnly => counts.disable_only += 1,
        DirectiveStatus::ParsedOnly => counts.parsed_only += 1,
        DirectiveStatus::SourceNoOp => counts.source_no_op += 1,
        DirectiveStatus::SourceBug => counts.source_bug += 1,
        DirectiveStatus::Deprecated => counts.deprecated += 1,
        DirectiveStatus::PlatformLimited => counts.platform_limited += 1,
    }
}

/// Validates one directive after nginx tokenization.
///
/// # Errors
///
/// Returns an error for unknown keys, invalid contexts, arity, or values.
pub fn validate_directive(
    name: &str,
    context: DirectiveContext,
    args: &[&str],
) -> Result<&'static DirectiveSpec, DirectiveError> {
    let spec = DIRECTIVES
        .iter()
        .find(|spec| spec.name == name)
        .ok_or_else(|| DirectiveError::UnknownDirective(name.to_owned()))?;

    if !spec.contexts.contains(&context) {
        return Err(DirectiveError::InvalidContext {
            name: spec.name,
            context,
        });
    }
    if args.len() < usize::from(spec.min_args)
        || spec
            .max_args
            .is_some_and(|max| args.len() > usize::from(max))
    {
        return Err(DirectiveError::InvalidArity {
            name: spec.name,
            expected: expected_arity(spec),
            actual: args.len(),
        });
    }

    validate_value(spec, args)?;
    Ok(spec)
}

fn expected_arity(spec: &DirectiveSpec) -> String {
    match spec.max_args {
        Some(max) if max == spec.min_args => spec.min_args.to_string(),
        Some(max) => format!("{}..={max}", spec.min_args),
        None => format!("at least {}", spec.min_args),
    }
}

fn validate_value(spec: &DirectiveSpec, args: &[&str]) -> Result<(), DirectiveError> {
    let valid = match spec.value_kind {
        ValueKind::AccessLog => args[0] != "off" || args.len() == 1,
        ValueKind::AccessRule => valid_access_rule(args),
        ValueKind::Bitmask(values) => args.iter().all(|arg| values.contains(arg)),
        ValueKind::Block => true,
        ValueKind::Command | ValueKind::HlsVariant | ValueKind::NamedBlock => !args[0].is_empty(),
        ValueKind::Duration => valid_duration(args[0]),
        ValueKind::DurationOrOff => args[0] == "off" || valid_duration(args[0]),
        ValueKind::Enum(values) => values.contains(&args[0]),
        ValueKind::Flag => matches!(args[0], "on" | "off"),
        ValueKind::Integer => args[0].parse::<u64>().is_ok(),
        ValueKind::Listen => valid_listen(args),
        ValueKind::LogFormat | ValueKind::Path | ValueKind::Strings => {
            args.iter().all(|arg| !arg.is_empty())
        }
        ValueKind::RelayTarget(kind) => valid_relay_target(kind, args),
        ValueKind::Signal => valid_signal(args[0]),
        ValueKind::Size => valid_size(args[0]),
        ValueKind::Url => args[0].starts_with("http://") && args[0].len() > "http://".len(),
    };

    if valid {
        Ok(())
    } else {
        Err(DirectiveError::InvalidValue {
            name: spec.name,
            detail: args.join(" "),
        })
    }
}

fn valid_access_rule(args: &[&str]) -> bool {
    match args {
        [target] => valid_access_target(target),
        [operation, target] => {
            matches!(*operation, "publish" | "play") && valid_access_target(target)
        }
        _ => false,
    }
}

fn valid_access_target(target: &str) -> bool {
    if target == "all" {
        return true;
    }
    let Some((address, prefix)) = target.split_once('/') else {
        return target.parse::<IpAddr>().is_ok();
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    prefix <= if address.is_ipv4() { 32 } else { 128 }
}

fn valid_listen(args: &[&str]) -> bool {
    if args[0].is_empty() {
        return false;
    }
    let Some(option) = args.get(1) else {
        return true;
    };

    if matches!(
        *option,
        "bind"
            | "ipv6only=on"
            | "ipv6only=off"
            | "so_keepalive=on"
            | "so_keepalive=off"
            | "proxy_protocol"
    ) {
        return true;
    }

    option.strip_prefix("so_keepalive=").is_some_and(|value| {
        let parts: Vec<_> = value.split(':').collect();
        parts.len() == 3 && parts.iter().all(|part| valid_seconds_duration(part))
    })
}

fn valid_relay_target(kind: RelayKind, args: &[&str]) -> bool {
    if args[0].is_empty() {
        return false;
    }

    let mut has_name = false;
    let mut is_static = false;
    for option in &args[1..] {
        let (key, value) = option
            .split_once('=')
            .map_or((*option, None), |(key, value)| (key, Some(value)));

        let valid = match key {
            "app" | "tcUrl" | "pageUrl" | "swfUrl" | "flashVer" | "playPath" => value.is_some(),
            "name" => {
                has_name = value.is_some_and(|value| !value.is_empty());
                has_name
            }
            "live" | "start" | "stop" => {
                value.is_none() || value.is_some_and(|value| value.parse::<u64>().is_ok())
            }
            "static" => {
                is_static = true;
                value.is_none() || value.is_some_and(|value| value.parse::<u64>().is_ok())
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }

    match kind {
        RelayKind::Push => !is_static,
        RelayKind::Pull => !is_static || has_name,
    }
}

fn valid_signal(value: &str) -> bool {
    const SIGNALS: &[&str] = &[
        "HUP", "INT", "QUIT", "ILL", "ABRT", "FPE", "KILL", "SEGV", "PIPE", "ALRM", "TERM", "USR1",
        "USR2", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU",
    ];

    value.parse::<u32>().is_ok() || SIGNALS.contains(&value)
}

fn valid_size(value: &str) -> bool {
    let (number, suffix) = value
        .char_indices()
        .last()
        .filter(|(_, last)| last.is_ascii_alphabetic())
        .map_or((value, None), |(index, last)| {
            (&value[..index], Some(last.to_ascii_lowercase()))
        });

    !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && suffix.is_none_or(|suffix| matches!(suffix, 'k' | 'm' | 'g'))
}

fn valid_duration(value: &str) -> bool {
    valid_nginx_time(value, false)
}

fn valid_seconds_duration(value: &str) -> bool {
    valid_nginx_time(value, true)
}

fn valid_nginx_time(value: &str, seconds_resolution: bool) -> bool {
    if value.is_empty() || !value.is_ascii() {
        return false;
    }

    let bytes = value.as_bytes();
    let mut index = 0;
    let mut previous_unit = if seconds_resolution { 0 } else { 2 };
    while index < bytes.len() {
        let number_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if number_start == index {
            return false;
        }
        if index == bytes.len() {
            return true;
        }

        let unit = if bytes[index..].starts_with(b"ms") {
            if seconds_resolution {
                return false;
            }
            index += 2;
            8
        } else {
            let unit = match bytes[index] {
                b'y' => 1,
                b'M' => 2,
                b'w' => 3,
                b'd' => 4,
                b'h' => 5,
                b'm' => 6,
                b's' => 7,
                _ => return false,
            };
            index += 1;
            unit
        };
        if unit <= previous_unit {
            return false;
        }
        previous_unit = unit;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::{valid_duration, valid_seconds_duration, valid_size};

    #[test]
    fn validates_nginx_size_and_time_literals() {
        assert!(valid_size("0"));
        assert!(valid_size("1K"));
        assert!(valid_size("4m"));
        assert!(!valid_size("1T"));

        assert!(valid_duration("0"));
        assert!(valid_duration("250ms"));
        assert!(valid_duration("1h30m"));
        assert!(!valid_duration("1m2h"));
        assert!(!valid_duration("1ms2s"));
        assert!(!valid_duration("1M"));
        assert!(valid_seconds_duration("1M2w"));
        assert!(!valid_seconds_duration("1ms"));
        assert!(!valid_duration("soon"));
    }
}
