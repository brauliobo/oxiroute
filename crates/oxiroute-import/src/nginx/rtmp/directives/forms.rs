#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const N: &[DirectiveContext] = &[NginxMain];
pub(super) const R: &[DirectiveContext] = &[RtmpMain];
pub(super) const S: &[DirectiveContext] = &[RtmpServer];
pub(super) const A: &[DirectiveContext] = &[RtmpApplication];
pub(super) const RS: &[DirectiveContext] = &[RtmpMain, RtmpServer];
pub(super) const RSA: &[DirectiveContext] = &[RtmpMain, RtmpServer, RtmpApplication];
pub(super) const RSAC: &[DirectiveContext] = &[RtmpMain, RtmpServer, RtmpApplication, RtmpRecorder];
pub(super) const H: &[DirectiveContext] = &[Http];

pub(super) const META: &[&str] = &["off", "on", "copy"];
pub(super) const NOTIFY_METHODS: &[&str] = &["get", "post"];
pub(super) const RECORD_PARTS: &[&str] = &["off", "all", "audio", "video", "keyframes", "manual"];
pub(super) const HLS_NAMING: &[&str] = &["sequential", "timestamp", "system"];
pub(super) const HLS_SLICING: &[&str] = &["plain", "aligned"];
pub(super) const HLS_TYPES: &[&str] = &["live", "event"];
pub(super) const STAT_PARTS: &[&str] = &["all", "global", "live", "clients"];
pub(super) const CONTROL_PARTS: &[&str] = &["all", "record", "drop", "redirect"];

pub(super) const RTMP_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "canonical rtmp block",
    contexts: N,
    status: Enforced,
}];
pub(super) const SERVER_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "canonical server block",
    contexts: R,
    status: Enforced,
}];
pub(super) const LISTENER_FORMS: &[DirectiveForm] = &[
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
pub(super) const APPLICATION_FORMS: &[DirectiveForm] = &[
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
pub(super) const CHUNK_SIZE_FORMS: &[DirectiveForm] = &[
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
pub(super) const ACK_WINDOW_FORMS: &[DirectiveForm] = &[
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
pub(super) const MAX_MESSAGE_FORMS: &[DirectiveForm] = &[
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
pub(super) const LIVE_FORMS: &[DirectiveForm] = &[
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
pub(super) const IDLE_STREAMS_FORMS: &[DirectiveForm] = &[
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
pub(super) const PUSH_FORMS: &[DirectiveForm] = &[
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
pub(super) const PULL_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "pull target",
    contexts: A,
    status: ParsedOnly,
}];
pub(super) const EXEC_FORMS: &[DirectiveForm] = &[
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
pub(super) const RESPAWN_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_PATH_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_SUFFIX_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_UNIQUE_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_INTERVAL_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_APPEND_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_LOCK_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_MAX_SIZE_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_MAX_FRAMES_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORD_NOTIFY_FORMS: &[DirectiveForm] = &[
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
pub(super) const RECORDER_FORMS: &[DirectiveForm] = &[
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
pub(super) const ACCESS_LOG_FORMS: &[DirectiveForm] = &[
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
pub(super) const ACCESS_RULE_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "bounded publish/play IP rule",
    contexts: RSA,
    status: Enforced,
}];
pub(super) const MAX_CONNECTIONS_FORMS: &[DirectiveForm] = &[
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
pub(super) const STREAM_BUCKETS_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered stream_buckets integer",
    contexts: RSA,
    status: StatusSourceBug,
}];
pub(super) const HLS_MUXDELAY_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered hls_muxdelay duration",
    contexts: RSA,
    status: StatusSourceNoOp,
}];
pub(super) const HLS_FORMS: &[DirectiveForm] = &[
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
pub(super) const DEPRECATED_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered so_keepalive flag",
    contexts: RS,
    status: StatusDeprecated,
}];
pub(super) const PLATFORM_EXEC_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered platform-owned value",
    contexts: RSA,
    status: StatusPlatformLimited,
}];
pub(super) const PLATFORM_NGINX_FORMS: &[DirectiveForm] = &[DirectiveForm {
    form: "any registered platform-owned value",
    contexts: N,
    status: StatusPlatformLimited,
}];
