use super::{MAX_SAFE_INTEGER, test_retry_policy};
use oxiroute_config::{
    HttpAccessPolicy, HttpAffinity, HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader,
    HttpPathSelector, HttpPoolSelect, HttpPoolSelectRule, HttpProxyPathRewrite, HttpProxyPolicy,
    HttpProxyTimeouts, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpRescuePolicy, HttpRescueTarget, HttpResponseHeaderMutation, HttpRetryPolicy, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpSameSite, HttpSelectInvalid, HttpSelectSpill,
    HttpService, HttpStaticMimePolicy, HttpStaticPathMapping, HttpUpstreamHost, L4Service,
    RtmpAccessPolicy, RtmpApplication, RtmpRecorder, RtmpRecorderSegmentNaming, RtmpRecorderStart,
    RtmpRecorderTimeBasis, RtmpRecorderTimezone, RtmpService, RtmpSessionCeilings, UdpPolicy,
};

fn test_proxy_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
        upstream_host: HttpUpstreamHost::Endpoint {
            unix_fallback: Some("fallback.internal:443".into()),
        },
        upstream_path_rewrite: Some(HttpProxyPathRewrite {
            from: "/api/".into(),
            to: "/application/".into(),
        }),
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "X-Authority".into(),
                value: HttpRequestHeaderValue::IncomingAuthority,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Literal".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "edge".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Host".into(),
                value: HttpRequestHeaderValue::NormalizedHost,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Client-Ip".into(),
                value: HttpRequestHeaderValue::ClientIp,
            },
            HttpRequestHeaderMutation::Set {
                name: "X-Upstream".into(),
                value: HttpRequestHeaderValue::SelectedUpstreamHost,
            },
            HttpRequestHeaderMutation::Remove {
                name: "X-Remove".into(),
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "X-Frame-Options".into(),
                value: "same-origin".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Remove {
                name: "X-Remove".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/".into(),
            to: "/application".into(),
            mode: oxiroute_config::CookiePathMatch::Exact,
        }],
        response_cookie_attributes: Vec::new(),
        retry: test_retry_policy(),
        timeouts: HttpProxyTimeouts {
            connect_ms: Some(500),
            header_ms: Some(2_000),
            idle_ms: Some(1_500),
            total_ms: Some(9_000),
        },
        rescue: Some(HttpRescuePolicy {
            statuses: vec![404, 503],
            target: HttpRescueTarget::Pool,
            pool: Some("web-backends".into()),
        }),
        select: Some(HttpPoolSelect {
            header: "x-min-vram".into(),
            rules: vec![HttpPoolSelectRule {
                at_most: 12,
                pool: "web-backends".into(),
            }],
            on_invalid: HttpSelectInvalid::Reject,
            forward: true,
            spill: HttpSelectSpill::Up,
        }),
        queue_class: Some("interactive".into()),
        affinity: Some(HttpAffinity::Cookie {
            name: "oxr_srv".into(),
            ttl_s: 600,
            same_site: HttpSameSite::Strict,
            secure: true,
            http_only: false,
        }),
        cache: None,
        queue_headers: true,
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn test_http_services() -> Vec<HttpService> {
    vec![
        HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: vec![
                HttpRoute {
                    host: Some(HttpHostSelector::NormalizedHost {
                        value: "API.EXAMPLE.TEST:443".into(),
                    }),
                    path: HttpPathSelector::SegmentPrefix {
                        value: "/api%3azone/".into(),
                    },
                    methods: vec!["POST".into(), "GET".into()],
                    access_policy: Some(HttpAccessPolicy::BearerTokenFile {
                        token_file_path: "/run/secrets/api-token".into(),
                        header_name: "X-Api-Token".into(),
                        realm: Some("private-api".into()),
                    }),
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::Proxy {
                        upstream_pool: "secure-backends".into(),
                        policy: test_proxy_policy().into(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::Exact {
                        value: "/health".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 200,
                        body: "healthy\n".into(),
                        headers: vec![HttpLiteralHeader {
                            name: "X-Source".into(),
                            value: "oxiroute".into(),
                            always: true,
                        }],
                    },
                },
                HttpRoute {
                    host: Some(HttpHostSelector::ExactAuthority {
                        value: "Example.TEST:8443".into(),
                    }),
                    path: HttpPathSelector::RawPrefix {
                        value: "/legacy".into(),
                    },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::Redirect {
                        status: 308,
                        location: HttpRedirectLocation::RequestTemplate {
                            value: "$scheme://$host$request_uri".into(),
                            nginx_host_fallback: None,
                        },
                        headers: Vec::new(),
                    },
                },
                HttpRoute {
                    host: None,
                    path: HttpPathSelector::SegmentPrefix {
                        value: "/assets".into(),
                    },
                    methods: vec!["GET".into()],
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::StaticFiles {
                        root_directory: "/srv//www/application".into(),
                        path_mapping: HttpStaticPathMapping::Root,
                        index_files: vec!["index.html".into()],
                        internal_index_redirects: false,
                        directory_redirects: false,
                        spa_fallback: Some("application/index.html".into()),
                        try_files: Vec::new(),
                        autoindex: false,
                        autoindex_exact_size: true,
                        autoindex_local_time: false,
                        etag: false,
                        mime: HttpStaticMimePolicy::default(),
                        headers: Vec::new(),
                        error_responses: Vec::new(),
                    },
                },
            ],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: Some(10 * 1024 * 1024),
            gzip: None,
            access_log: None,
        },
        HttpService {
            drain_deadline_ms: None,
            name: "fallback".into(),
            routes: vec![HttpRoute {
                host: Some(HttpHostSelector::NormalizedHost {
                    value: "*.EXAMPLE.TEST".into(),
                }),
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: vec!["HEAD".into()],
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "web-backends".into(),
                    policy: HttpProxyPolicy {
                        upstream_host: HttpUpstreamHost::Literal {
                            value: "backend.internal".into(),
                        },
                        upstream_path_rewrite: None,
                        request_headers: Vec::new(),
                        response_headers: Vec::new(),
                        response_cookie_path_rewrites: Vec::new(),
                        response_cookie_attributes: Vec::new(),
                        retry: HttpRetryPolicy::default(),
                        timeouts: HttpProxyTimeouts::default(),
                        rescue: None,
                        select: None,
                        affinity: None,
                        queue_class: None,
                        cache: None,
                        queue_headers: false,
                    }
                    .into(),
                },
            }],
            automatic_response_headers: false,
            upstream_io_timeout_ms: 30_000,
            max_request_body_bytes: None,
            gzip: None,
            access_log: None,
        },
    ]
}

pub(crate) fn test_l4_services() -> Vec<L4Service> {
    vec![
        L4Service {
            drain_deadline_ms: None,
            name: "database".into(),
            upstream_pool: "database-backends".into(),
            connect_timeout_ms: MAX_SAFE_INTEGER - 2,
            idle_timeout_ms: MAX_SAFE_INTEGER - 3,
            lifetime_timeout_ms: Some(MAX_SAFE_INTEGER - 4),
            proxy_protocol: None,
            udp: Some(UdpPolicy {
                max_datagram_bytes: 1_200,
                max_sessions: 2_048,
                max_session_bytes: 8 * 1024 * 1024,
                max_queue_datagrams: 32,
                max_queue_bytes: 64 * 1024,
            }),
        },
        L4Service {
            drain_deadline_ms: None,
            name: "database-short-lived".into(),
            upstream_pool: "database-backends".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 2_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        },
    ]
}

pub(crate) fn test_rtmp_services() -> Vec<RtmpService> {
    vec![RtmpService {
        name: "live".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: true,
            publish: RtmpAccessPolicy::default(),
            play: RtmpAccessPolicy::default(),
            limits: RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy {
                max_subscribers: 1_024,
                max_queue_messages_per_subscriber: 256,
                max_queue_bytes_per_subscriber: 8 * 1_024 * 1_024,
            },
            vod: None,
            hls: None,
            dash: None,
            recorders: vec![
                RtmpRecorder {
                    name: "continuous".into(),
                    start: RtmpRecorderStart::Continuous,
                    root_directory: "/var//lib/oxiroute/recordings".into(),
                    record_mask: oxiroute_config::RtmpRecordMask::default(),
                    suffix_template: "-%Y-%m-%dT%H-%M-%S.flv".into(),
                    append_unix_seconds: true,
                    append: false,
                    lock: false,
                    max_size: None,
                    max_frames: None,
                    notify: false,
                    timezone: RtmpRecorderTimezone::Utc,
                    time_basis: RtmpRecorderTimeBasis::SegmentStart,
                    segment_naming: RtmpRecorderSegmentNaming::SafeUnique,
                    rotation_interval_ms: Some(60_000),
                    max_queue_messages: 256,
                    max_queue_bytes: 8 * 1024 * 1024,
                    shutdown_timeout_ms: 5_000,
                    max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                    max_storage_files: Some(10_000),
                    max_active_recorders: 8,
                },
                RtmpRecorder {
                    name: "manual".into(),
                    start: RtmpRecorderStart::Manual,
                    root_directory: "/var/lib/oxiroute/recordings".into(),
                    record_mask: oxiroute_config::RtmpRecordMask::default(),
                    suffix_template: ".flv".into(),
                    append_unix_seconds: false,
                    append: false,
                    lock: false,
                    max_size: None,
                    max_frames: None,
                    notify: false,
                    timezone: RtmpRecorderTimezone::Iana("America/Argentina/Buenos_Aires".into()),
                    time_basis: RtmpRecorderTimeBasis::SegmentEnd,
                    segment_naming: RtmpRecorderSegmentNaming::NginxCompatible,
                    rotation_interval_ms: None,
                    max_queue_messages: 64,
                    max_queue_bytes: 1024 * 1024,
                    shutdown_timeout_ms: 3_000,
                    max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                    max_storage_files: Some(10_000),
                    max_active_recorders: 8,
                },
            ],
        }],
    }]
}
