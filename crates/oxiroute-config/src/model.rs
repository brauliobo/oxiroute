use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};

use crate::defaults::{
    MAX_CERTIFICATE_DNS_NAMES, MAX_CERTIFICATES, MAX_ENDPOINTS_PER_POOL,
    MAX_RTMP_APPLICATIONS_PER_SERVICE, MAX_RTMP_DASH_OUTPUTS, MAX_RTMP_HLS_OUTPUTS,
    MAX_RTMP_RECORDERS_PER_APPLICATION, MAX_RTMP_RECORDING_ROOTS, MAX_RTMP_SERVICES,
    MAX_SOURCE_BYTES, MAX_TLS_PROFILES, MAX_TOTAL_ENDPOINTS, MAX_TOTAL_RTMP_RECORDERS,
    default_acme_dns01_timeout_seconds, default_acme_retained_revisions,
    default_acme_retention_days, default_cache_grace_ms, default_cache_keep_ms,
    default_cache_max_bytes, default_cache_max_entries, default_cache_max_followers_per_fill,
    default_cache_max_header_bytes, default_cache_max_in_flight_fills, default_cache_max_key_bytes,
    default_cache_max_object_bytes, default_cache_max_tag_bytes, default_cache_max_tags_per_object,
    default_cache_methods, default_cache_ttl_ms, default_connect_timeout_ms,
    default_disk_cache_max_bytes, default_disk_cache_max_files, default_forward_connect_ports,
    default_forward_lifetime_timeout_ms, default_forward_max_connections,
    default_forward_max_header_bytes, default_forward_peer_max_retries,
    default_forward_resolver_cache_entries, default_forward_resolver_concurrent_queries,
    default_forward_resolver_max_addresses, default_forward_resolver_max_ttl_ms,
    default_forward_resolver_min_ttl_ms, default_forward_resolver_negative_ttl_ms,
    default_health_interval_ms, default_health_timeout_ms, default_healthy_threshold,
    default_http_access_header_name, default_http_redirect_status, default_http_static_index_files,
    default_idle_timeout_ms, default_max_request_body_bytes, default_passive_error_limit,
    default_passive_initial_backoff_ms, default_passive_max_backoff_ms,
    default_passive_recovery_threshold, default_proxy_protocol_timeout_ms,
    default_recorder_max_active_recorders, default_recorder_max_queue_bytes,
    default_recorder_max_queue_messages, default_recorder_shutdown_timeout_ms,
    default_recorder_suffix_template, default_rtmp_ack_window_size,
    default_rtmp_auto_push_connect_timeout_ms, default_rtmp_auto_push_handshake_timeout_ms,
    default_rtmp_auto_push_max_peers, default_rtmp_auto_push_max_queue_bytes,
    default_rtmp_auto_push_max_queue_messages, default_rtmp_auto_push_max_streams,
    default_rtmp_auto_push_reconnect_ms, default_rtmp_callback_timeout_ms,
    default_rtmp_callback_update_timeout_ms, default_rtmp_dash_max_active_streams,
    default_rtmp_dash_max_queue_messages, default_rtmp_dash_max_segment_bytes,
    default_rtmp_dash_max_segment_duration_ms, default_rtmp_dash_max_storage_bytes,
    default_rtmp_dash_max_storage_files, default_rtmp_dash_segment_duration_ms,
    default_rtmp_exec_max_processes, default_rtmp_exec_max_queue_bytes,
    default_rtmp_exec_max_queue_messages, default_rtmp_exec_max_respawns,
    default_rtmp_exec_max_stderr_bytes, default_rtmp_exec_max_stdout_bytes,
    default_rtmp_exec_respawn_delay_ms, default_rtmp_exec_shutdown_timeout_ms,
    default_rtmp_exec_timeout_ms, default_rtmp_hls_max_active_streams,
    default_rtmp_hls_max_queue_messages, default_rtmp_hls_max_segment_bytes,
    default_rtmp_hls_max_segment_duration_ms, default_rtmp_hls_max_storage_bytes,
    default_rtmp_hls_max_storage_files, default_rtmp_hls_playlist_length_ms,
    default_rtmp_hls_segment_duration_ms, default_rtmp_max_chain_depth,
    default_rtmp_max_inbound_message_size, default_rtmp_outbound_chunk_size,
    default_rtmp_pull_reconnect_ms, default_rtmp_push_reconnect_ms, default_rtmp_relay_buffer_ms,
    default_rtmp_relay_connect_timeout_ms, default_rtmp_relay_dns_refresh_ms,
    default_rtmp_relay_handshake_timeout_ms, default_rtmp_relay_queue_bytes,
    default_rtmp_relay_queue_messages, default_rtmp_vod_duration_ms, default_rtmp_vod_file_bytes,
    default_rtmp_vod_sessions, default_self_signed_validity_days, default_true,
    default_udp_max_datagram_bytes, default_udp_max_queue_bytes, default_udp_max_queue_datagrams,
    default_udp_max_session_bytes, default_udp_max_sessions, default_unhealthy_threshold,
    default_upstream_io_timeout_ms,
};

pub(crate) const fn default_http_route_policy() -> HttpRoutePolicy {
    HttpRoutePolicy::new()
}

pub(crate) const fn default_rtmp_fanout_policy() -> RtmpFanoutPolicy {
    RtmpFanoutPolicy {
        max_subscribers: crate::defaults::DEFAULT_RTMP_MAX_SUBSCRIBERS,
        max_queue_messages_per_subscriber: default_rtmp_relay_queue_messages(),
        max_queue_bytes_per_subscriber: default_rtmp_relay_queue_bytes(),
    }
}

pub(crate) const fn default_rtmp_session_ceilings() -> RtmpSessionCeilings {
    RtmpSessionCeilings {
        max_connections: crate::defaults::DEFAULT_RTMP_APPLICATION_CONNECTIONS,
        max_publishers: crate::defaults::DEFAULT_RTMP_APPLICATION_PUBLISHERS,
        max_viewers: crate::defaults::DEFAULT_RTMP_APPLICATION_VIEWERS,
    }
}

pub(crate) const fn default_rtmp_relay_policy() -> RtmpRelayPolicy {
    RtmpRelayPolicy {
        max_queue_messages: default_rtmp_relay_queue_messages(),
        max_queue_bytes: default_rtmp_relay_queue_bytes(),
        buffer_ms: default_rtmp_relay_buffer_ms(),
        push_reconnect_ms: default_rtmp_push_reconnect_ms(),
        pull_reconnect_ms: default_rtmp_pull_reconnect_ms(),
        dns_refresh_ms: default_rtmp_relay_dns_refresh_ms(),
        connect_timeout_ms: default_rtmp_relay_connect_timeout_ms(),
        handshake_timeout_ms: default_rtmp_relay_handshake_timeout_ms(),
    }
}

pub(crate) fn default_rtmp_auto_push_policy() -> RtmpAutoPushPolicy {
    RtmpAutoPushPolicy {
        enabled: false,
        socket_dir: PathBuf::from("/tmp/oxiroute-rtmp"),
        secret_file: None,
        reconnect_ms: default_rtmp_auto_push_reconnect_ms(),
        connect_timeout_ms: default_rtmp_auto_push_connect_timeout_ms(),
        handshake_timeout_ms: default_rtmp_auto_push_handshake_timeout_ms(),
        max_peers: default_rtmp_auto_push_max_peers(),
        max_queue_messages: default_rtmp_auto_push_max_queue_messages(),
        max_queue_bytes: default_rtmp_auto_push_max_queue_bytes(),
        max_streams: default_rtmp_auto_push_max_streams(),
    }
}

pub(crate) const fn default_rtmp_outbound_policy() -> RtmpOutboundPolicy {
    RtmpOutboundPolicy {
        allow_domains: Vec::new(),
        deny_domains: Vec::new(),
        allow_cidrs: Vec::new(),
        deny_cidrs: Vec::new(),
        deny_private: true,
        rtmps: RtmpRtmpsPolicy::Disabled,
        max_chain_depth: default_rtmp_max_chain_depth(),
    }
}

pub(crate) fn default_alpn() -> Vec<AlpnProtocol> {
    vec![AlpnProtocol::Http11]
}

pub(crate) fn default_http_retry_triggers() -> Vec<HttpRetryTrigger> {
    vec![
        HttpRetryTrigger::ConnectFailure,
        HttpRetryTrigger::ConnectTimeout,
        HttpRetryTrigger::RefusedStream,
    ]
}

pub(crate) fn default_cache_key_components() -> Vec<CacheKeyComponent> {
    vec![
        CacheKeyComponent::Scheme,
        CacheKeyComponent::NormalizedHost,
        CacheKeyComponent::PathAndQuery,
    ]
}

pub(crate) fn default_forward_http_versions() -> Vec<ForwardHttpVersion> {
    vec![ForwardHttpVersion::H1]
}

mod core;
mod errors;
mod forward;
mod http;
mod rtmp;

pub use core::*;
pub use errors::*;
pub use forward::*;
pub use http::*;
pub use rtmp::*;
