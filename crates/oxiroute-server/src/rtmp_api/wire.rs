pub(super) const fn relay_phase(phase: oxiroute_rtmp::RtmpRelayPhase) -> &'static str {
    match phase {
        oxiroute_rtmp::RtmpRelayPhase::Connecting => "connecting",
        oxiroute_rtmp::RtmpRelayPhase::Publishing => "publishing",
        oxiroute_rtmp::RtmpRelayPhase::Pulling => "pulling",
        oxiroute_rtmp::RtmpRelayPhase::Backoff => "backoff",
        oxiroute_rtmp::RtmpRelayPhase::Stopped => "stopped",
    }
}

pub(super) const fn relay_failure(failure: oxiroute_rtmp::RtmpRelayFailure) -> &'static str {
    match failure {
        oxiroute_rtmp::RtmpRelayFailure::Policy => "policy",
        oxiroute_rtmp::RtmpRelayFailure::Connect => "connect",
        oxiroute_rtmp::RtmpRelayFailure::Handshake => "handshake",
        oxiroute_rtmp::RtmpRelayFailure::Session => "session",
        oxiroute_rtmp::RtmpRelayFailure::Transport => "transport",
        oxiroute_rtmp::RtmpRelayFailure::Source => "source",
        oxiroute_rtmp::RtmpRelayFailure::Thread => "thread",
    }
}

pub(super) const fn relay_dns_refresh_failure(
    failure: oxiroute_rtmp::RtmpDnsRefreshFailure,
) -> &'static str {
    match failure {
        oxiroute_rtmp::RtmpDnsRefreshFailure::Resolution => "resolution",
        oxiroute_rtmp::RtmpDnsRefreshFailure::AddressSet => "address_set",
        oxiroute_rtmp::RtmpDnsRefreshFailure::Policy => "policy",
        oxiroute_rtmp::RtmpDnsRefreshFailure::DirectLoop => "direct_loop",
        oxiroute_rtmp::RtmpDnsRefreshFailure::FamilyMismatch => "family_mismatch",
    }
}
