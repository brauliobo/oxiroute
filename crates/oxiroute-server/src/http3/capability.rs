#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn capability_snapshot(
    listeners: &[crate::ListenerSnapshot],
    mode: RuntimeMode,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "supervision": {
            "mode": mode,
            "descriptorAdoption": {
                "status": if mode == RuntimeMode::Supervised { "negotiated" } else { "not_used" },
                "manifestVersion": 1,
                "datagram": true,
                "quic": true,
            },
        },
        "udp": udp_listener_capability(listeners),
        "http3": {
            "reverse": listener_capability(listeners, "http3"),
            "forward": listener_capability(listeners, "forward_http3"),
        },
    })
}

fn udp_listener_capability(listeners: &[crate::ListenerSnapshot]) -> serde_json::Value {
    let configured = listeners
        .iter()
        .filter(|listener| listener.protocol == "udp")
        .collect::<Vec<_>>();
    let active = configured
        .iter()
        .filter(|listener| {
            listener.state == ListenerRuntimeState::Listening
                && listener.administrative_state == crate::AdministrativeState::Ready
        })
        .map(|listener| listener.name.clone())
        .collect::<Vec<_>>();
    let status = if !active.is_empty() {
        "active"
    } else if configured.is_empty() {
        "unconfigured"
    } else {
        "blocked"
    };
    let blocked_reason = if status != "blocked" {
        None
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Failed)
    {
        Some("listener_runtime_failed")
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Stopped)
    {
        Some("listener_stopped")
    } else {
        Some("listener_not_listening")
    };
    serde_json::json!({
        "status": status,
        "supported": true,
        "listeners": active,
        "configuredListeners": configured.iter().map(|listener| listener.name.clone()).collect::<Vec<_>>(),
        "transport": "udp",
        "drain": "graceful",
        "fallback": "none",
        "blockedReason": blocked_reason,
    })
}

fn listener_capability(listeners: &[crate::ListenerSnapshot], protocol: &str) -> serde_json::Value {
    let configured = listeners
        .iter()
        .filter(|listener| listener.protocol == protocol)
        .collect::<Vec<_>>();
    let active = configured
        .iter()
        .filter(|listener| {
            listener.state == ListenerRuntimeState::Listening
                && listener.administrative_state == crate::AdministrativeState::Ready
        })
        .map(|listener| listener.name.clone())
        .collect::<Vec<_>>();
    let status = if !active.is_empty() {
        "active"
    } else if configured.is_empty() {
        "unconfigured"
    } else {
        "blocked"
    };
    let blocked_reason = if status != "blocked" {
        None
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Failed)
    {
        Some("listener_runtime_failed")
    } else if configured
        .iter()
        .any(|listener| listener.state == ListenerRuntimeState::Stopped)
    {
        Some("listener_stopped")
    } else {
        Some("listener_not_listening")
    };
    serde_json::json!({
        "status": status,
        "supported": true,
        "listeners": active,
        "configuredListeners": configured.iter().map(|listener| listener.name.clone()).collect::<Vec<_>>(),
        "transport": "quic",
        "alpn": ["h3"],
        "tlsMinVersion": "1.3",
        "zeroRtt": "disabled",
        "migration": "disabled",
        "goAway": "graceful",
        "fallback": "none",
        "unsupported": ["compression", "upgrades"],
        "limits": {
            "maxHandshakesAndConnections": H3_HANDSHAKE_LIMIT,
            "maxBidirectionalStreams": H3_BIDI_STREAM_LIMIT,
            "maxUnidirectionalStreams": H3_UNI_STREAM_LIMIT,
            "maxFieldSectionBytes": H3_MAX_FIELD_SECTION_BYTES,
            "maxRequestBodyBytes": H3_MAX_REQUEST_BODY_BYTES,
            "maxResponseBodyBytes": H3_MAX_RESPONSE_BODY_BYTES,
        },
        "blockedReason": blocked_reason,
    })
}
