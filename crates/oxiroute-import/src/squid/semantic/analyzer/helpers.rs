#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const fn resolution_for(
    semantics: DirectiveSemantics,
    activation: Activation,
) -> DirectiveResolution {
    if matches!(activation, Activation::Externalized) {
        return DirectiveResolution::Externalized;
    }
    match semantics {
        DirectiveSemantics::Include => {
            if matches!(activation, Activation::Structural) {
                DirectiveResolution::Structural
            } else {
                DirectiveResolution::Blocked
            }
        }
        DirectiveSemantics::AclSource
        | DirectiveSemantics::AclPort
        | DirectiveSemantics::AclProxyAuth => DirectiveResolution::MergeSameName,
        DirectiveSemantics::HttpAccess
        | DirectiveSemantics::HeaderAccess
        | DirectiveSemantics::DirectAccess
        | DirectiveSemantics::CacheAccess
        | DirectiveSemantics::RefreshPattern => DirectiveResolution::OrderedFirstMatch,
        DirectiveSemantics::HttpPort
        | DirectiveSemantics::HttpsPort
        | DirectiveSemantics::IcpPort
        | DirectiveSemantics::HtcpPort
        | DirectiveSemantics::CachePeer
        | DirectiveSemantics::AccessLogging
        | DirectiveSemantics::DnsNameservers
        | DirectiveSemantics::StorageSetting => DirectiveResolution::Append,
        DirectiveSemantics::AuthenticationHelper
        | DirectiveSemantics::AuthenticationRealm
        | DirectiveSemantics::AuthenticationCredentialTtl
        | DirectiveSemantics::AuthenticationSetting
        | DirectiveSemantics::LoggingSetting
        | DirectiveSemantics::DnsSetting
        | DirectiveSemantics::ForwardedFor
        | DirectiveSemantics::Via
        | DirectiveSemantics::HeaderPrivacy
        | DirectiveSemantics::CacheSetting
        | DirectiveSemantics::CoreDumpDirectory
        | DirectiveSemantics::ProcessSetting => DirectiveResolution::LastWins,
        DirectiveSemantics::AclUnsupported | DirectiveSemantics::Unknown => {
            DirectiveResolution::Blocked
        }
    }
}

pub(super) fn origin(expanded: &ExpandedDirective) -> DirectiveOrigin {
    DirectiveOrigin {
        occurrence: expanded.occurrence,
        directive_span: expanded.directive.span,
        name_span: expanded.directive.name.span,
        argument_spans: expanded
            .directive
            .arguments
            .iter()
            .map(|word| word.span)
            .collect(),
        provenance: expanded.provenance.clone(),
    }
}

pub(super) fn builtin_acl(value: &[u8]) -> Option<BuiltinAcl> {
    match value {
        b"all" => Some(BuiltinAcl::All),
        b"CONNECT" => Some(BuiltinAcl::Connect),
        b"localhost" => Some(BuiltinAcl::Localhost),
        b"manager" => Some(BuiltinAcl::Manager),
        b"to_localhost" => Some(BuiltinAcl::ToLocalhost),
        b"to_linklocal" => Some(BuiltinAcl::ToLinkLocal),
        _ => None,
    }
}

pub(super) fn proxy_auth_matcher(value: &Word) -> AclMatcher {
    if value.value == b"REQUIRED" {
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Required)
    } else {
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Identity(SecretFact {
            kind: SecretKind::ProxyIdentity,
            span: value.span,
        }))
    }
}

pub(super) fn access_kind(name: &[u8]) -> Option<AccessListKind> {
    match name {
        b"http_access" => Some(AccessListKind::Http),
        b"follow_x_forwarded_for" => Some(AccessListKind::FollowForwardedFor),
        b"always_direct" => Some(AccessListKind::AlwaysDirect),
        b"never_direct" => Some(AccessListKind::NeverDirect),
        b"cache" | b"no_cache" => Some(AccessListKind::Cache),
        b"cache_peer_access" => Some(AccessListKind::CachePeer),
        name if name.ends_with(b"_access") => Some(AccessListKind::Other),
        _ => None,
    }
}

pub(super) const fn access_semantics(kind: AccessListKind) -> DirectiveSemantics {
    match kind {
        AccessListKind::Http => DirectiveSemantics::HttpAccess,
        AccessListKind::RequestHeader
        | AccessListKind::ReplyHeader
        | AccessListKind::FollowForwardedFor
        | AccessListKind::Other => DirectiveSemantics::HeaderAccess,
        AccessListKind::AlwaysDirect | AccessListKind::NeverDirect => {
            DirectiveSemantics::DirectAccess
        }
        AccessListKind::Cache | AccessListKind::CachePeer => DirectiveSemantics::CacheAccess,
    }
}

pub(super) const fn access_blocker(kind: AccessListKind) -> SemanticBlockerKind {
    match kind {
        AccessListKind::Http => SemanticBlockerKind::OrderedHttpAccess,
        AccessListKind::RequestHeader
        | AccessListKind::ReplyHeader
        | AccessListKind::FollowForwardedFor
        | AccessListKind::Other => SemanticBlockerKind::HeaderAccessPolicy,
        AccessListKind::AlwaysDirect | AccessListKind::NeverDirect => {
            SemanticBlockerKind::DirectRoutingPolicy
        }
        AccessListKind::Cache | AccessListKind::CachePeer => SemanticBlockerKind::CacheAccessPolicy,
    }
}

pub(super) const fn port_semantics(kind: PortKind) -> DirectiveSemantics {
    match kind {
        PortKind::Http => DirectiveSemantics::HttpPort,
        PortKind::Https => DirectiveSemantics::HttpsPort,
        PortKind::Icp => DirectiveSemantics::IcpPort,
        PortKind::Htcp => DirectiveSemantics::HtcpPort,
    }
}

pub(super) fn parse_port_option(option: &Word) -> PortOption {
    match option.value.as_slice() {
        b"intercept" => PortOption::Intercept,
        b"tproxy" => PortOption::Tproxy,
        b"accel" => PortOption::Accel,
        b"ssl-bump" => PortOption::SslBump,
        value if bytes::assignment(value, b"name").is_some() => PortOption::Name(option.into()),
        value if bytes::assignment(value, b"defaultsite").is_some() => {
            PortOption::DefaultSite(option.into())
        }
        _ => PortOption::Unsupported(option.into()),
    }
}

pub(super) fn parse_peer_option(option: &Word) -> PeerOption {
    if let Some(kind) = peer_secret_kind(&option.value) {
        return PeerOption::Secret(SecretFact {
            kind,
            span: option.span,
        });
    }
    match option.value.as_slice() {
        b"no-query" => PeerOption::NoQuery,
        b"proxy-only" => PeerOption::ProxyOnly,
        b"originserver" => PeerOption::OriginServer,
        b"round-robin" => PeerOption::RoundRobin,
        value if bytes::assignment(value, b"weight").is_some() => {
            bytes::assignment(value, b"weight")
                .and_then(bytes::unsigned)
                .map_or_else(
                    || PeerOption::Unsupported(option.into()),
                    PeerOption::Weight,
                )
        }
        value if bytes::assignment(value, b"name").is_some() => PeerOption::Name(option.into()),
        _ => PeerOption::Unsupported(option.into()),
    }
}

pub(super) fn is_static_parent_peer(peer: &CachePeer) -> bool {
    peer.peer_type == CachePeerType::Parent
        && peer.http_port != 0
        && peer.icp_port == 0
        && peer.options.is_empty()
        && (std::str::from_utf8(&peer.host.value)
            .ok()
            .and_then(|value| value.parse::<std::net::IpAddr>().ok())
            .is_some()
            || dns_name(&peer.host.value).is_some())
}

fn peer_secret_kind(value: &[u8]) -> Option<SecretKind> {
    if [b"login".as_slice(), b"password", b"passwd"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PeerCredentials)
    } else if [b"token".as_slice(), b"bearer"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::BearerToken)
    } else if [b"key".as_slice(), b"private_key"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PrivateKey)
    } else if [b"password_hash".as_slice(), b"passwordhash"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PasswordHash)
    } else {
        None
    }
}

pub(super) fn parse_refresh_option(option: &Word) -> Option<RefreshOption> {
    match option.value.as_slice() {
        b"override-expire" => Some(RefreshOption::OverrideExpire),
        b"override-lastmod" => Some(RefreshOption::OverrideLastModified),
        b"reload-into-ims" => Some(RefreshOption::ReloadIntoIms),
        b"ignore-reload" => Some(RefreshOption::IgnoreReload),
        b"ignore-no-store" => Some(RefreshOption::IgnoreNoStore),
        b"ignore-private" => Some(RefreshOption::IgnorePrivate),
        b"refresh-ims" => Some(RefreshOption::RefreshIms),
        b"store-stale" => Some(RefreshOption::StoreStale),
        value => bytes::assignment(value, b"max-stale")
            .and_then(bytes::unsigned)
            .map(RefreshOption::MaxStale),
    }
}

pub(super) fn parse_log_destination(word: &Word) -> LogDestination {
    match word.value.as_slice() {
        b"none" => LogDestination::Disabled,
        value if value.starts_with(b"stdio:") => LogDestination::Stdio(word.into()),
        value if value.starts_with(b"daemon:") => LogDestination::Daemon(word.into()),
        b"syslog" => LogDestination::Syslog(None),
        value if value.starts_with(b"syslog:") => LogDestination::Syslog(Some(word.into())),
        _ => LogDestination::File(word.into()),
    }
}

pub(super) fn is_cache_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_mem"
            | b"maximum_object_size"
            | b"minimum_object_size"
            | b"maximum_object_size_in_memory"
            | b"cache_replacement_policy"
            | b"memory_replacement_policy"
            | b"offline_mode"
            | b"collapsed_forwarding"
            | b"cache_swap_low"
            | b"cache_swap_high"
            | b"store_avg_object_size"
            | b"quick_abort_min"
            | b"quick_abort_max"
            | b"quick_abort_pct"
            | b"read_ahead_gap"
            | b"range_offset_limit"
    )
}

pub(super) fn is_storage(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_dir"
            | b"store_dir_select_algorithm"
            | b"cache_swap_state"
            | b"unlinkd_program"
            | b"store_id_program"
            | b"store_id_children"
    )
}

pub(super) fn is_authentication(name: &[u8]) -> bool {
    matches!(
        name,
        b"authenticate_cache_garbage_interval"
            | b"authenticate_ttl"
            | b"authenticate_ip_ttl"
            | b"external_acl_type"
            | b"url_rewrite_program"
            | b"sslpassword_program"
    )
}

pub(super) fn is_logging(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_log"
            | b"cache_store_log"
            | b"logfile_daemon"
            | b"logformat"
            | b"debug_options"
            | b"log_icp_queries"
            | b"buffered_logs"
            | b"strip_query_terms"
    )
}

pub(super) fn is_dns(name: &[u8]) -> bool {
    matches!(
        name,
        b"dns_v4_first"
            | b"dns_timeout"
            | b"positive_dns_ttl"
            | b"negative_dns_ttl"
            | b"ipcache_size"
            | b"fqdncache_size"
            | b"hosts_file"
    )
}

pub(super) fn is_process(name: &[u8]) -> bool {
    matches!(
        name,
        b"coredump_dir"
            | b"pid_filename"
            | b"workers"
            | b"max_filedescriptors"
            | b"cache_effective_user"
            | b"cache_effective_group"
            | b"visible_hostname"
            | b"unique_hostname"
    )
}
