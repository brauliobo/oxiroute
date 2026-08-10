use std::{
    fmt::{Display, Write as _},
    path::Path,
};

use crate::{
    defaults::MAX_SOURCE_BYTES,
    model::{
        AccessLogPolicy, AlpnProtocol, CacheAuthorizationPolicy, CacheKeyComponent, CachePredicate,
        CachePurgeAuthorization, CacheSetCookiePolicy, CacheStaleTrigger, CacheStatusTtl,
        CacheStore, CacheStoreCommon, CacheStoreKind, CacheSurrogateTags, CacheVaryPolicy,
        Certificate, CertificateSource, Config, ConfigError, DnsResolutionPolicy,
        DownstreamTimeoutPolicy, ForwardAccessAction, ForwardAccessMatcher, ForwardAccessPolicy,
        ForwardAccessRule, ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy,
        ForwardDirectFallback, ForwardHeaderPolicy, ForwardHttpVersion, ForwardPeerPolicy,
        ForwardProxyAuth, ForwardProxyService, ForwardResolverPolicy, ForwardTimeRange,
        ForwardViaPolicy, ForwardWeekday, ForwardedForPolicy, HealthCheck, HealthCheckType,
        HealthHttpVersion, HealthStartup, HttpAccessPolicy, HttpCachePolicy,
        HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpGzipPolicy, HttpHostSelector,
        HttpLiteralHeader, HttpMimeType, HttpPathSelector, HttpProxyPathRewrite, HttpProxyPolicy,
        HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
        HttpResponseHeaderMutation, HttpRetryBodySafety, HttpRetryMethodSafety, HttpRetryPolicy,
        HttpRetryTarget, HttpRetryTrigger, HttpRoute, HttpRouteAction, HttpRoutePolicy,
        HttpSameSite, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
        HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost, HttpVersion, HttpVersionPolicy,
        L4Service, Listener, ListenerBind, Management, PassiveHealthPolicy, PassiveObserve,
        PassiveOnError, Protocol, ProxyProtocolPolicy, ProxyProtocolVersion, RtmpAccessPolicy,
        RtmpAccessRule, RtmpAclAction, RtmpApplication, RtmpCallbackConfig, RtmpDashPolicy,
        RtmpDashSegmentNaming, RtmpExecEnvironment, RtmpExecFilesystemPolicy, RtmpExecMode,
        RtmpExecNetworkPolicy, RtmpExecProfile, RtmpExecTrigger, RtmpFanoutPolicy,
        RtmpHlsFragmentNaming, RtmpHlsKeyPolicy, RtmpHlsPolicy, RtmpHlsVariant, RtmpNotifyMethod,
        RtmpPullTarget, RtmpPushTarget, RtmpRecorder, RtmpRecorderSegmentNaming, RtmpRecorderStart,
        RtmpRecorderTimeBasis, RtmpRecorderTimezone, RtmpRelayPolicy, RtmpRtmpsPolicy, RtmpService,
        RtmpSessionCeilings, RtmpTokenPolicy, RtmpTokenSource, RtmpTransport, RtmpVodPolicy,
        RtmpVodSource, Stats, StatsPage, StatsPageAdminPolicy, TlsProfile, TlsVersion, UdpPolicy,
        UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
        UpstreamTls,
    },
    validation::validate_config,
};

/// Renders a validated, normalized configuration as deterministic data-only Lua.
///
/// Every current field is emitted, including defaults, empty collections, and explicit `nil` or
/// `null` options. Collection order is preserved.
///
/// # Errors
///
/// Returns an error when the configuration is invalid, contains a non-UTF-8 path, or renders beyond
/// the loader's source-size limit.
pub fn render_lua(config: &Config) -> Result<String, ConfigError> {
    let mut config = config.clone();
    validate_config(&mut config)?;

    let mut renderer = Renderer::new();
    renderer.config(&config)?;
    let output = renderer.finish();
    if output.len() > MAX_SOURCE_BYTES {
        return Err(ConfigError::SourceTooLarge);
    }

    Ok(output)
}

struct Renderer {
    output: String,
    indent: usize,
}

include!("render/common.rs");
include!("render/rtmp.rs");
include!("render/upstream.rs");
include!("render/http.rs");
include!("render/forward.rs");
include!("render/writer.rs");
