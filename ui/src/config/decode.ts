import {
  arrayOf,
  integerInRange,
  isRecord,
  isString,
  nullableSafeInteger,
  nullableString,
  safeInteger,
} from '../valueGuards'
import { HTTP_RETRY_TRIGGERS, UPSTREAM_WEIGHT_MAX, UPSTREAM_WEIGHT_MIN } from './model'
import type {
  ListenerProtocol,
  TlsVersion,
  AlpnProtocol,
  HttpVersion,
  HealthCheckType,
  WeightedRoundRobinAlgorithm,
  UpstreamAlgorithm,
  RtmpRecorderStart,
  RtmpAclAction,
  RtmpTokenSource,
  RtmpNotifyMethod,
  RtmpRtmpsPolicy,
  RtmpTransport,
  RtmpExecMode,
  RtmpExecTrigger,
  RtmpExecFilesystemPolicy,
  RtmpExecNetworkPolicy,
  RtmpVodSource,
  AccessLogConfig,
  ManagementConfig,
  StatsConfig,
  StatsPageConfig,
  DirectCertificateSource,
  CertbotCertificateSource,
  AcmeChallengeType,
  AcmeKeyType,
  AcmeDns01Config,
  AcmeManagedCertificateSource,
  SelfSignedKeyType,
  SelfSignedDevelopmentCertificateSource,
  CertificateSource,
  CertificateConfig,
  TlsProfileConfig,
  TlsPolicyConfig,
  TlsClientAuthMode,
  TlsClientAuthPolicyConfig,
  TlsSessionCacheConfig,
  SocketListenerBind,
  UnixListenerBind,
  UdpListenerBind,
  ListenerBind,
  ProxyProtocolVersion,
  ProxyProtocolPolicyConfig,
  ListenerConfig,
  MemoryCacheStoreConfig,
  DiskCacheStoreConfig,
  CacheStoreConfig,
  HealthCheckConfig,
  PassiveHealthConfig,
  UpstreamTlsConfig,
  HttpVersionPolicyConfig,
  SocketUpstreamEndpoint,
  DnsUpstreamEndpoint,
  UnixUpstreamEndpoint,
  UpstreamEndpoint,
  UpstreamServerConfig,
  UpstreamPoolConfig,
  HttpHostKind,
  HttpPathKind,
  HttpRetryTrigger,
  HttpHostSelectorConfig,
  HttpPathSelectorConfig,
  HttpBearerTokenFileAccessConfig,
  HttpBasicHtpasswdFileAccessConfig,
  HttpAccessPolicyConfig,
  HttpUpstreamHostConfig,
  HttpRequestHeaderValueConfig,
  HttpRequestHeaderMutationConfig,
  HttpResponseHeaderMutationConfig,
  HttpCookiePathRewriteConfig,
  HttpProxyPathRewriteConfig,
  HttpCookieAttributePolicyConfig,
  HttpRetryPolicyConfig,
  HttpProxyPolicyConfig,
  CacheKeyComponentConfig,
  CacheStatusTtlConfig,
  CacheStaleTrigger,
  CachePredicateConfig,
  CacheSurrogateTagsConfig,
  CachePurgeAuthorizationConfig,
  HttpCachePolicyConfig,
  HttpLiteralHeaderConfig,
  HttpRedirectLocationConfig,
  HttpProxyActionConfig,
  HttpFixedResponseActionConfig,
  HttpRedirectActionConfig,
  HttpStaticFilesActionConfig,
  HttpRouteActionConfig,
  HttpRouteConfig,
  HttpServiceConfig,
  RtmpHlsFragmentNaming,
  RtmpHlsVariantConfig,
  RtmpHlsKeyConfig,
  RtmpHlsPolicyConfig,
  RtmpDashPolicyConfig,
  RtmpApplicationConfig,
  RtmpAccessPolicyConfig,
  RtmpAccessRuleConfig,
  RtmpTokenPolicyConfig,
  RtmpSessionCeilingsConfig,
  RtmpRecorderConfig,
  RtmpCallbackConfig,
  RtmpOutboundPolicyConfig,
  RtmpRelayPolicyConfig,
  RtmpCredentialReferenceConfig,
  RtmpPushTargetConfig,
  RtmpPullTargetConfig,
  RtmpVodPolicyConfig,
  RtmpExecEnvironmentConfig,
  RtmpExecProfileConfig,
  RtmpAutoPushConfig,
  RtmpServiceConfig,
  ForwardHttpVersion,
  ForwardConnectPolicyConfig,
  ForwardPeerConfig,
  ForwardPeerPolicyConfig,
  ForwardProxyAuthConfig,
  ForwardPortRangeConfig,
  ForwardAccessMatcherConfig,
  ForwardAccessConditionConfig,
  ForwardAccessPolicyConfig,
  ForwardDestinationPolicyConfig,
  ForwardTimeRangeConfig,
  ForwardWeekday,
  ForwardResolverPolicyConfig,
  ForwardProxyServiceConfig,
  L4ServiceConfig,
  UdpPolicyConfig,
  CanonicalConfig,
  DiagnosticSeverity,
  DiagnosticStage,
  ConfigDiagnostic,
  ConfigFormat,
  ConfigSnapshot,
  ConfigSaveOutcome,
  ConfigActivationState,
  ConfigSaveResponse,
  ConfigRequest,
} from './model'

export function isCanonicalConfig(value: unknown): value is CanonicalConfig {
  if (!isRecord(value) || !safeInteger(value.version) || !nullableSafeInteger(value.max_connections) ||
    !(value.management === null || (isRecord(value.management) &&
      typeof value.management.bind === 'string' && nullableString(value.management.ui_dir))) ||
    !(value.stats === undefined || value.stats === null || (isRecord(value.stats) &&
      arrayOf(value.stats.binds, isString) && nullableString(value.stats.admin_token_file) &&
      arrayOf(value.stats.pages, isStatsPage))) ||
    !arrayOf(value.certificates, isCertificate) || !arrayOf(value.tls_profiles, isTlsProfile) ||
    !arrayOf(value.listeners, isListener) || !arrayOf(value.cache_stores, isCacheStore) ||
    !arrayOf(value.upstream_pools, isUpstreamPool) || !arrayOf(value.http_services, isHttpService) ||
    !arrayOf(value.forward_proxy_services, isForwardProxyService) ||
    !arrayOf(value.rtmp_services, isRtmpService) ||
    !arrayOf(value.l4_services, isL4Service)
  ) return false
  return true
}

export function isConfigDiagnostic(value: unknown): value is ConfigDiagnostic {
  return isRecord(value) && typeof value.code === 'string' &&
    ['error', 'warning'].includes(String(value.severity)) &&
    ['read', 'parse', 'validation', 'render', 'conflict', 'write', 'sync', 'rollback', 'activation']
      .includes(String(value.stage)) && typeof value.message === 'string' &&
    (value.path === undefined || typeof value.path === 'string')
}

export function errorDiagnosticsFrom(value: unknown): ConfigDiagnostic[] {
  return isRecord(value) && Array.isArray(value.diagnostics)
    ? (value.diagnostics as ConfigDiagnostic[])
    : []
}

function isCertificate(value: unknown): value is CertificateConfig {
  if (!isRecord(value) || typeof value.name !== 'string' || !arrayOf(value.dns_names, isString) ||
    !isRecord(value.source)) return false
  const source = value.source
  if (source.type === 'files') {
    return typeof source.certificate_chain_path === 'string' && typeof source.private_key_path === 'string'
  }
  if (source.type === 'certbot') {
    return typeof source.live_directory_path === 'string' && typeof source.archive_directory_path === 'string'
  }
  if (source.type === 'acme_managed') {
    return typeof source.directory_url === 'string' && typeof source.state_root === 'string' &&
      arrayOf(source.contacts, isString) && typeof source.terms_agreed === 'boolean' &&
      ['http01', 'dns01', 'tls_alpn01'].includes(String(source.challenge)) &&
      ['ecdsa_p256', 'rsa_2048'].includes(String(source.key_type)) &&
      arrayOf(source.allowed_dns_suffixes, isString) &&
      safeInteger(source.retained_revisions) && safeInteger(source.retention_days) &&
      (source.dns01 === null || (isRecord(source.dns01) &&
        typeof source.dns01.provider === 'string' &&
        typeof source.dns01.credential_file === 'string' &&
        safeInteger(source.dns01.timeout_seconds)))
  }
  return source.type === 'self_signed_development' && safeInteger(source.validity_days) &&
    ['ecdsa_p256', 'rsa_2048'].includes(String(source.key_type))
}

function isTlsProfile(value: unknown): value is TlsProfileConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.certificates, isString) &&
    typeof value.default_certificate === 'string' && ['1.2', '1.3'].includes(String(value.min_version)) &&
    arrayOf(value.alpn, (entry) => ['h3', 'h2', 'http/1.1'].includes(String(entry))) &&
    isRecord(value.policy) && nullableString(value.policy.cipher_list) &&
    nullableString(value.policy.dh_parameters_path) &&
    isRecord(value.policy.client_auth) && ['disabled', 'optional', 'required'].includes(String(value.policy.client_auth.mode)) &&
    nullableString(value.policy.client_auth.ca_certificate_path) &&
    arrayOf(value.policy.client_auth.allowed_dns_names, isString) &&
    (value.policy.session_cache === null || (isRecord(value.policy.session_cache) &&
      typeof value.policy.session_cache.name === 'string' && safeInteger(value.policy.session_cache.size_bytes))) &&
    nullableSafeInteger(value.policy.session_timeout_seconds) &&
    typeof value.policy.session_tickets === 'boolean' && typeof value.policy.prefer_server_ciphers === 'boolean'
}

function isListener(value: unknown): value is ListenerConfig {
  return isRecord(value) && typeof value.name === 'string' && isListenerBind(value.bind) &&
    ['http', 'tcp', 'rtmp', 'forward_http1', 'forward_http2', 'forward_http3']
      .includes(String(value.protocol)) && nullableString(value.service) &&
    nullableString(value.tls_profile) &&
    (value.proxy_protocol === undefined || value.proxy_protocol === null || isProxyProtocolPolicy(value.proxy_protocol)) &&
    nullableSafeInteger(value.max_connections) &&
    isRecord(value.downstream_timeouts) &&
    nullableSafeInteger(value.downstream_timeouts.client_timeout_ms) &&
    nullableSafeInteger(value.downstream_timeouts.request_timeout_ms) &&
    nullableSafeInteger(value.downstream_timeouts.keepalive_timeout_ms)
}

function isListenerBind(value: unknown): value is ListenerBind {
  return isRecord(value) && (value.type === 'socket' || value.type === 'udp'
    ? typeof value.address === 'string'
    : value.type === 'unix' && typeof value.path === 'string' && nullableSafeInteger(value.mode))
}

function isCacheStore(value: unknown): value is CacheStoreConfig {
  if (!isRecord(value) || typeof value.name !== 'string' || !safeInteger(value.max_bytes) ||
    !safeInteger(value.max_object_bytes) || !safeInteger(value.max_header_bytes) ||
    !safeInteger(value.max_key_bytes) || !safeInteger(value.max_tag_bytes) ||
    !safeInteger(value.max_tags_per_object) || !safeInteger(value.max_in_flight_fills) ||
    !safeInteger(value.max_followers_per_fill)
  ) return false
  return value.type === 'memory'
    ? safeInteger(value.max_entries)
    : value.type === 'disk' && typeof value.root_directory === 'string' && safeInteger(value.max_files)
}

function isProxyProtocolPolicy(value: unknown): value is ProxyProtocolPolicyConfig {
  return isRecord(value) && ['v1', 'v2', 'auto'].includes(String(value.version)) &&
    integerInRange(value.timeout_ms, 1, 60_000)
}

function isUpstreamPool(value: unknown): value is UpstreamPoolConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.servers, isUpstreamServer) &&
    (value.endpoints === undefined || arrayOf(value.endpoints, isEndpoint)) &&
    isUpstreamAlgorithm(value.algorithm, value.servers.length) &&
    (value.health_check === null || isHealthCheck(value.health_check)) &&
    (value.passive_health === null || isPassiveHealth(value.passive_health)) &&
    (value.tls === null || (isRecord(value.tls) && typeof value.tls.server_name === 'string' &&
      nullableString(value.tls.ca_certificate_path))) && isRecord(value.http_versions) &&
    ['1.1', '2'].includes(String(value.http_versions.min)) &&
    ['1.1', '2'].includes(String(value.http_versions.max)) &&
    nullableSafeInteger(value.queue_timeout_ms) && nullableSafeInteger(value.connect_timeout_ms) &&
    nullableSafeInteger(value.server_timeout_ms) &&
    ['never', 'safe', 'always'].includes(String(value.connection_reuse))
}

function isUpstreamAlgorithm(value: unknown, serverCount: number): value is UpstreamAlgorithm {
  if (['round_robin', 'least_connections', 'first'].includes(String(value))) return true
  return isRecord(value) && value.type === 'weighted_round_robin' &&
    Array.isArray(value.weights) && value.weights.length === serverCount &&
    value.weights.every((weight) => integerInRange(weight, UPSTREAM_WEIGHT_MIN, UPSTREAM_WEIGHT_MAX))
}

function isUpstreamServer(value: unknown): value is UpstreamServerConfig {
  return isRecord(value) && typeof value.name === 'string' && isEndpoint(value.endpoint) &&
    nullableSafeInteger(value.max_connections) &&
    ['startup', 'on_connect'].includes(String(value.dns_resolution))
}

function isEndpoint(value: unknown): value is UpstreamEndpoint {
  return isRecord(value) && (value.type === 'socket'
    ? typeof value.address === 'string'
    : value.type === 'dns'
      ? typeof value.host === 'string' && safeInteger(value.port)
      : value.type === 'unix' && typeof value.path === 'string')
}

function isHealthCheck(value: unknown): value is HealthCheckConfig {
  return isRecord(value) && ['http', 'tcp'].includes(String(value.type)) &&
    safeInteger(value.interval_ms) && safeInteger(value.timeout_ms) &&
    safeInteger(value.healthy_threshold) && safeInteger(value.unhealthy_threshold) &&
    ['healthy', 'unhealthy', 'checking'].includes(String(value.startup)) &&
    nullableSafeInteger(value.fast_interval_ms) && nullableSafeInteger(value.down_interval_ms) &&
    nullableString(value.host) && nullableString(value.path) &&
    nullableSafeInteger(value.expected_status) &&
    (value.http_version === null || ['1.0', '1.1'].includes(String(value.http_version)))
}

function isPassiveHealth(value: unknown): value is PassiveHealthConfig {
  if (!isRecord(value) || !['layer4', 'layer7'].includes(String(value.observe)) ||
    !['count', 'immediately', 'mark_down'].includes(String(value.on_error)) ||
    !integerInRange(value.error_limit, 1, 100) || typeof value.mark_down !== 'boolean' ||
    typeof value.mark_up !== 'boolean' || !integerInRange(value.initial_backoff_ms, 1, 86_400_000) ||
    !integerInRange(value.max_backoff_ms, 1, 86_400_000) ||
    !integerInRange(value.recovery_threshold, 1, 100)) return false
  return value.max_backoff_ms >= value.initial_backoff_ms
}

function isStatsPage(value: unknown): value is StatsPageConfig {
  return isRecord(value) && typeof value.bind === 'string' && typeof value.uri_prefix === 'string' &&
    integerInRange(value.refresh_ms, 1, 86_400_000) &&
    ['disabled', 'localhost'].includes(String(value.admin)) &&
    nullableSafeInteger(value.max_connections) && isRecord(value.downstream_timeouts) &&
    nullableSafeInteger(value.downstream_timeouts.client_timeout_ms) &&
    nullableSafeInteger(value.downstream_timeouts.request_timeout_ms) &&
    nullableSafeInteger(value.downstream_timeouts.keepalive_timeout_ms)
}

function isHttpService(value: unknown): value is HttpServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.routes, (route) =>
    isRecord(route) && (route.host === null || isHttpHost(route.host)) && isHttpPath(route.path) &&
    arrayOf(route.methods, isString) && (route.access_policy === null || isHttpAccess(route.access_policy)) &&
    isHttpRoutePolicy(route.policy) && isHttpAction(route.action)) &&
    typeof value.automatic_response_headers === 'boolean' && safeInteger(value.upstream_io_timeout_ms) &&
    nullableSafeInteger(value.max_request_body_bytes) &&
    (value.gzip === null || (isRecord(value.gzip) && safeInteger(value.gzip.level) &&
      arrayOf(value.gzip.content_types, isString) && safeInteger(value.gzip.min_length_bytes) &&
      ['1.0', '1.1'].includes(String(value.gzip.min_http_version)) &&
      typeof value.gzip.disable_on_via === 'boolean' && typeof value.gzip.vary === 'boolean')) &&
    (value.access_log === null || isAccessLog(value.access_log))
}

function isHttpRoutePolicy(value: unknown): boolean {
  return isRecord(value) && nullableSafeInteger(value.max_request_body_bytes) &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.read_timeout_ms) &&
    safeInteger(value.write_timeout_ms) && typeof value.request_buffering === 'boolean' &&
    typeof value.response_buffering === 'boolean'
}

function isHttpHost(value: unknown): value is HttpHostSelectorConfig {
  return isRecord(value) && ['normalized_host', 'exact_authority',
    'ascii_case_insensitive_exact_authority', 'nginx_leading_wildcard',
    'nginx_leading_dot'].includes(String(value.kind)) &&
    typeof value.value === 'string'
}

function isHttpPath(value: unknown): value is HttpPathSelectorConfig {
  return isRecord(value) && ['segment_prefix', 'raw_prefix', 'exact'].includes(String(value.kind)) &&
    typeof value.value === 'string'
}

function isHttpAccess(value: unknown): value is HttpAccessPolicyConfig {
  return isRecord(value) && (value.type === 'bearer_token_file'
    ? typeof value.token_file_path === 'string' && typeof value.header_name === 'string' &&
      nullableString(value.realm)
    : value.type === 'basic_htpasswd_file' && typeof value.htpasswd_file_path === 'string' &&
      typeof value.realm === 'string')
}

function isHttpAction(value: unknown): value is HttpRouteActionConfig {
  if (!isRecord(value)) return false
  switch (value.type) {
    case 'proxy':
      return typeof value.upstream_pool === 'string' && isHttpProxyPolicy(value.policy)
    case 'fixed_response':
      return integerInRange(value.status, 200, 599) && typeof value.body === 'string' &&
        arrayOf(value.headers, isLiteralHeader)
    case 'redirect':
      return [301, 302, 307, 308].includes(Number(value.status)) &&
        isHttpRedirectLocation(value.location) && arrayOf(value.headers, isLiteralHeader)
    case 'static_files':
      return typeof value.root_directory === 'string' && arrayOf(value.index_files, isString) &&
        typeof value.internal_index_redirects === 'boolean' &&
        typeof value.directory_redirects === 'boolean' &&
        nullableString(value.spa_fallback) && ['root', 'alias'].includes(String(value.path_mapping)) &&
        arrayOf(value.try_files, isStaticTryFile) && typeof value.autoindex === 'boolean' &&
        typeof value.autoindex_exact_size === 'boolean' &&
        typeof value.autoindex_local_time === 'boolean' &&
        isRecord(value.mime) && nullableString(value.mime.default_type) &&
        arrayOf(value.mime.types, (entry) => isRecord(entry) && typeof entry.extension === 'string' &&
          typeof entry.content_type === 'string') && arrayOf(value.headers, isLiteralHeader) &&
        arrayOf(value.error_responses, (entry) => isRecord(entry) &&
          arrayOf(entry.statuses, safeInteger) && nullableString(entry.file) &&
          nullableString(entry.body) && arrayOf(entry.headers, isLiteralHeader) &&
          nullableString(entry.internal_redirect))
    default:
      return false
  }
}

function isStaticTryFile(value: unknown): boolean {
  return isRecord(value) && (['request_path', 'request_path_directory'].includes(String(value.type)) ||
    (value.type === 'relative' && typeof value.path === 'string') ||
    (value.type === 'status' && safeInteger(value.status)))
}

function isLiteralHeader(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' && typeof value.value === 'string' &&
    (value.always === undefined || typeof value.always === 'boolean')
}

function isHttpRedirectLocation(value: unknown): value is HttpRedirectLocationConfig {
  return isRecord(value) && typeof value.value === 'string' && (value.kind === 'literal' ||
    (value.kind === 'request_template' && nullableString(value.nginx_host_fallback)))
}

function isHttpProxyPolicy(value: unknown): value is HttpProxyPolicyConfig {
  const responseStatuses = value && isRecord(value) && isRecord(value.retry)
    ? value.retry.response_statuses
    : undefined
  const hasResponseStatuses = Array.isArray(responseStatuses) && responseStatuses.length > 0
  return isRecord(value) && isHttpUpstreamHost(value.upstream_host) &&
    (value.upstream_path_rewrite === undefined || value.upstream_path_rewrite === null ||
      (isRecord(value.upstream_path_rewrite) && typeof value.upstream_path_rewrite.from === 'string' &&
        typeof value.upstream_path_rewrite.to === 'string')) &&
    arrayOf(value.request_headers, isRequestHeaderMutation) &&
    arrayOf(value.response_headers, isResponseHeaderMutation) &&
    arrayOf(value.response_cookie_path_rewrites, (rewrite) => isRecord(rewrite) &&
      typeof rewrite.from === 'string' && typeof rewrite.to === 'string') &&
    arrayOf(value.response_cookie_attributes, (policy) => isRecord(policy) &&
      typeof policy.name === 'string' && (policy.secure === null || typeof policy.secure === 'boolean') &&
      (policy.http_only === null || typeof policy.http_only === 'boolean') &&
      (policy.same_site === null || ['strict', 'lax', 'none'].includes(String(policy.same_site)))) &&
    isRecord(value.retry) && integerInRange(value.retry.max_retries, 0, 3) &&
    ['same_server', 'next_server'].includes(String(value.retry.target)) &&
    integerInRange(value.retry.delay_ms, 0, 60_000) &&
    typeof value.retry.final_redispatch === 'boolean' &&
    (!value.retry.final_redispatch ||
      (value.retry.max_retries > 0 && value.retry.target === 'same_server')) &&
    isHttpRetryStatuses(responseStatuses ?? []) &&
    isHttpRetryTriggers(value.retry.triggers, hasResponseStatuses || value.retry.max_retries === 0) &&
    value.retry.method_safety === 'get_head' && value.retry.body_safety === 'empty' &&
    (value.cache === null || isHttpCachePolicy(value.cache))
}

function isHttpCachePolicy(value: unknown): value is HttpCachePolicyConfig {
  return isRecord(value) && typeof value.store === 'string' && arrayOf(value.methods, isString) &&
    arrayOf(value.key_components, isCacheKeyComponent) &&
    typeof value.use_origin_cache_control === 'boolean' && safeInteger(value.default_ttl_ms) &&
    arrayOf(value.status_ttls, (entry) => isRecord(entry) && safeInteger(entry.status) &&
      safeInteger(entry.ttl_ms)) && safeInteger(value.grace_ms) && safeInteger(value.keep_ms) &&
    typeof value.revalidate === 'boolean' && typeof value.collapsed_forwarding === 'boolean' &&
    arrayOf(value.stale_on, (entry) => ['connect_failure', 'connect_timeout', 'origin_500',
      'origin_502', 'origin_503', 'origin_504'].includes(String(entry))) &&
    arrayOf(value.bypass_request, isCachePredicate) &&
    arrayOf(value.no_store_request, isCachePredicate) &&
    arrayOf(value.no_store_response, isCachePredicate) &&
    ['bypass', 'ignore'].includes(String(value.set_cookie_policy)) &&
    ['bypass', 'cache'].includes(String(value.authorization_policy)) &&
    ['respect', 'ignore'].includes(String(value.vary_policy)) &&
    (value.surrogate_tags === null || (isRecord(value.surrogate_tags) &&
      typeof value.surrogate_tags.response_header === 'string' &&
      safeInteger(value.surrogate_tags.max_tags) && safeInteger(value.surrogate_tags.max_tag_bytes))) &&
    (value.purge_authorization === null || (isRecord(value.purge_authorization) &&
      value.purge_authorization.type === 'bearer_token_file' &&
      typeof value.purge_authorization.token_file_path === 'string'))
}

function isCacheKeyComponent(value: unknown): value is CacheKeyComponentConfig {
  return isRecord(value) && (['scheme', 'normalized_host', 'path_and_query'].includes(String(value.type)) ||
    (['header', 'cookie'].includes(String(value.type)) && typeof value.name === 'string'))
}

function isCachePredicate(value: unknown): value is CachePredicateConfig {
  return isRecord(value) && ['header_present', 'cookie_present'].includes(String(value.type)) &&
    typeof value.name === 'string'
}

function isHttpRetryTriggers(value: unknown, allowEmpty = false): value is HttpRetryTrigger[] {
  return Array.isArray(value) && (allowEmpty || value.length > 0) &&
    value.every((trigger) => HTTP_RETRY_TRIGGERS.includes(trigger as HttpRetryTrigger)) &&
    new Set(value).size === value.length
}

function isHttpRetryStatuses(value: unknown): value is number[] {
  return Array.isArray(value) && value.length <= 100 &&
    value.every((status) => integerInRange(status, 500, 599)) &&
    new Set(value).size === value.length
}

function isHttpUpstreamHost(value: unknown): value is HttpUpstreamHostConfig {
  return isRecord(value) && (value.type === 'preserve_incoming' ||
    (value.type === 'nginx_host' && typeof value.fallback === 'string') ||
    (value.type === 'endpoint' && nullableString(value.unix_fallback)) ||
    (value.type === 'literal' && typeof value.value === 'string'))
}

function isRequestHeaderMutation(value: unknown): value is HttpRequestHeaderMutationConfig {
  return isRecord(value) && typeof value.name === 'string' && (value.operation === 'remove' ||
    (value.operation === 'set' && isRequestHeaderValue(value.value)))
}

function isRequestHeaderValue(value: unknown): value is HttpRequestHeaderValueConfig {
  if (!isRecord(value)) return false
  if (['incoming_authority', 'normalized_host', 'client_ip', 'downstream_scheme',
    'selected_upstream_host'].includes(String(value.type))) return true
  if (value.type === 'nginx_host') return typeof value.fallback === 'string'
  if (value.type === 'literal') return typeof value.value === 'string'
  if (value.type === 'appended_x_forwarded_for') {
    return safeInteger(value.max_bytes) &&
      arrayOf(value.except_source_cidrs, (cidr): cidr is string => typeof cidr === 'string') &&
      value.except_source_cidrs.length <= 16
  }
  return value.type === 'incoming_header' && typeof value.name === 'string' && safeInteger(value.max_bytes)
}

function isResponseHeaderMutation(value: unknown): value is HttpResponseHeaderMutationConfig {
  return isRecord(value) && typeof value.name === 'string' && (value.operation === 'remove' ||
    (['set', 'add'].includes(String(value.operation)) && typeof value.value === 'string' &&
      typeof value.always === 'boolean'))
}

function isRtmpService(value: unknown): value is RtmpServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && safeInteger(value.outbound_chunk_size) &&
    (value.access_log === null || isAccessLog(value.access_log)) && isRtmpOutboundPolicy(value.outbound_policy) &&
    isRtmpCallbackConfig(value.callbacks) && isRtmpAutoPushConfig(value.auto_push) &&
    (value.exec_profiles === undefined || arrayOf(value.exec_profiles, isRtmpExecProfile)) &&
    arrayOf(value.applications, (application) =>
    isRecord(application) && typeof application.name === 'string' && typeof application.live === 'boolean' &&
    typeof application.idle_streams === 'boolean' && isRtmpAccessPolicy(application.publish) &&
    isRtmpAccessPolicy(application.play) && isRtmpSessionCeilings(application.limits) &&
    arrayOf(application.push_targets, isRtmpPushTarget) && arrayOf(application.pull_targets, isRtmpPullTarget) &&
    isRtmpRelayPolicy(application.relay) && isRtmpCallbackConfig(application.callbacks) &&
    isRecord(application.fanout) &&
    safeInteger(application.fanout.max_subscribers) &&
    safeInteger(application.fanout.max_queue_messages_per_subscriber) &&
    safeInteger(application.fanout.max_queue_bytes_per_subscriber) &&
    (application.vod === null || isRtmpVodPolicy(application.vod)) &&
    (application.hls === undefined || application.hls === null || isRtmpHlsPolicy(application.hls)) &&
    (application.dash === undefined || application.dash === null || isRtmpDashPolicy(application.dash)) &&
    arrayOf(application.recorders, isRtmpRecorder))
}

function isRtmpAutoPushConfig(value: unknown): value is RtmpAutoPushConfig {
  return isRecord(value) && typeof value.enabled === 'boolean' && typeof value.socket_dir === 'string' &&
    nullableString(value.secret_file) && safeInteger(value.reconnect_ms) &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.handshake_timeout_ms) &&
    safeInteger(value.max_peers) && safeInteger(value.max_queue_messages) &&
    safeInteger(value.max_queue_bytes) && safeInteger(value.max_streams)
}

function isRtmpCallbackConfig(value: unknown): value is RtmpCallbackConfig {
  return isRecord(value) && nullableString(value.on_connect) && nullableString(value.on_disconnect) &&
    nullableString(value.on_publish) && nullableString(value.on_publish_done) && nullableString(value.on_play) &&
    nullableString(value.on_play_done) && nullableString(value.on_done) && nullableString(value.on_update) &&
    ['get', 'post'].includes(String(value.notify_method)) && safeInteger(value.timeout_ms) &&
    safeInteger(value.notify_update_timeout_ms) && typeof value.notify_update_strict === 'boolean' &&
    typeof value.notify_relay_redirect === 'boolean'
}

function isRtmpOutboundPolicy(value: unknown): value is RtmpOutboundPolicyConfig {
  return isRecord(value) && arrayOf(value.allow_domains, isString) && arrayOf(value.deny_domains, isString) &&
    arrayOf(value.allow_cidrs, isString) && arrayOf(value.deny_cidrs, isString) &&
    typeof value.deny_private === 'boolean' && ['disabled', 'allowed', 'required'].includes(String(value.rtmps)) &&
    safeInteger(value.max_chain_depth)
}

function isRtmpRelayPolicy(value: unknown): value is RtmpRelayPolicyConfig {
  return isRecord(value) && safeInteger(value.max_queue_messages) && safeInteger(value.max_queue_bytes) &&
    safeInteger(value.buffer_ms) && safeInteger(value.push_reconnect_ms) && safeInteger(value.pull_reconnect_ms) &&
    safeInteger(value.dns_refresh_ms) &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.handshake_timeout_ms)
}

function isRtmpCredentialReference(value: unknown): value is RtmpCredentialReferenceConfig {
  return isRecord(value) && typeof value.username === 'string' && typeof value.secret_file === 'string'
}

function isRtmpPushTarget(value: unknown): value is RtmpPushTargetConfig {
  return isRecord(value) && typeof value.host === 'string' && safeInteger(value.port) &&
    typeof value.application === 'string' && ['rtmp', 'rtmps'].includes(String(value.scheme)) &&
    nullableString(value.stream_name) && nullableString(value.tc_url) && nullableString(value.flash_version) &&
    (value.credentials === null || isRtmpCredentialReference(value.credentials))
}

function isRtmpPullTarget(value: unknown): value is RtmpPullTargetConfig {
  return isRecord(value) && typeof value.host === 'string' && safeInteger(value.port) &&
    typeof value.application === 'string' && typeof value.stream_name === 'string' &&
    ['rtmp', 'rtmps'].includes(String(value.scheme)) && nullableString(value.tc_url) &&
    nullableString(value.flash_version) && (value.credentials === null || isRtmpCredentialReference(value.credentials))
}

function isRtmpVodPolicy(value: unknown): value is RtmpVodPolicyConfig {
  return isRecord(value) && arrayOf(value.sources, isRtmpVodSource) && safeInteger(value.max_sessions) &&
    safeInteger(value.max_file_bytes) && safeInteger(value.max_duration_ms)
}

function isRtmpHlsPolicy(value: unknown): value is RtmpHlsPolicyConfig {
  return isRecord(value) && typeof value.root_directory === 'string' &&
    safeInteger(value.segment_duration_ms) && safeInteger(value.max_segment_duration_ms) &&
    safeInteger(value.playlist_length_ms) && ['sequential', 'timestamp', 'system'].includes(String(value.fragment_naming)) &&
    typeof value.nested === 'boolean' && typeof value.cleanup === 'boolean' &&
    arrayOf(value.variants, (variant) => isRecord(variant) && typeof variant.name === 'string' &&
      safeInteger(variant.bandwidth) && nullableString(variant.codecs) && nullableSafeInteger(variant.width) &&
      nullableSafeInteger(variant.height)) &&
    (value.keys === null || (isRecord(value.keys) && safeInteger(value.keys.rotation_segments) &&
      typeof value.keys.url_prefix === 'string')) && safeInteger(value.max_segment_bytes) &&
    safeInteger(value.max_queue_messages) && safeInteger(value.max_storage_bytes) &&
    safeInteger(value.max_storage_files) && safeInteger(value.max_active_streams)
}

function isRtmpDashPolicy(value: unknown): value is RtmpDashPolicyConfig {
  return isRecord(value) && typeof value.root_directory === 'string' &&
    safeInteger(value.segment_duration_ms) && safeInteger(value.max_segment_duration_ms) &&
    safeInteger(value.playlist_length_ms) && ['sequential', 'timestamp', 'system'].includes(String(value.segment_naming)) &&
    typeof value.nested === 'boolean' && typeof value.cleanup === 'boolean' &&
    safeInteger(value.max_segment_bytes) && safeInteger(value.max_queue_messages) &&
    safeInteger(value.max_storage_bytes) && safeInteger(value.max_storage_files) &&
    safeInteger(value.max_active_streams)
}

function isRtmpExecProfile(value: unknown): value is RtmpExecProfileConfig {
  return isRecord(value) && typeof value.name === 'string' && typeof value.application === 'string' &&
    ['command', 'transcode'].includes(String(value.mode)) && ['publisher', 'publish_done'].includes(String(value.trigger)) &&
    typeof value.executable === 'string' && arrayOf(value.arguments, isString) &&
    arrayOf(value.environment, (environment) => isRecord(environment) && typeof environment.name === 'string' &&
      typeof environment.value === 'string') && typeof value.working_directory === 'string' &&
    ['working_directory', 'host'].includes(String(value.filesystem)) &&
    ['disabled', 'inherited'].includes(String(value.network)) && safeInteger(value.timeout_ms) &&
    safeInteger(value.shutdown_timeout_ms) && safeInteger(value.max_processes) &&
    safeInteger(value.max_queue_messages) && safeInteger(value.max_queue_bytes) &&
    safeInteger(value.max_stdout_bytes) && safeInteger(value.max_stderr_bytes) &&
    typeof value.respawn === 'boolean' && safeInteger(value.respawn_delay_ms) &&
    safeInteger(value.max_respawns)
}

function isRtmpVodSource(value: unknown): value is RtmpVodSource {
  if (!isRecord(value) || typeof value.name !== 'string') return false
  if (value.type === 'local') return typeof value.root_directory === 'string'
  return value.type === 'http' && typeof value.origin === 'string'
}

function isRtmpAccessPolicy(value: unknown): value is RtmpAccessPolicyConfig {
  return isRecord(value) && arrayOf(value.rules, isRtmpAccessRule) &&
    (value.token === null || isRtmpTokenPolicy(value.token))
}

function isRtmpAccessRule(value: unknown): value is RtmpAccessRuleConfig {
  return isRecord(value) && ['allow', 'deny'].includes(String(value.action)) &&
    typeof value.network === 'string'
}

function isRtmpTokenPolicy(value: unknown): value is RtmpTokenPolicyConfig {
  return isRecord(value) && value.source === 'stream_query' &&
    typeof value.parameter === 'string' && value.parameter.length > 0 && value.parameter.length <= 32 &&
    typeof value.secret === 'string' && value.secret.length > 0 && value.secret.length <= 128
}

function isRtmpSessionCeilings(value: unknown): value is RtmpSessionCeilingsConfig {
  return isRecord(value) && integerInRange(value.max_connections, 1, 100_000) &&
    integerInRange(value.max_publishers, 1, 10_000) &&
    integerInRange(value.max_viewers, 1, 1_000_000)
}

function isAccessLog(value: unknown): value is AccessLogConfig {
  return isRecord(value) && (value.type === 'disabled' ||
    (value.type === 'file' && typeof value.path === 'string'))
}

function isForwardProxyService(value: unknown): value is ForwardProxyServiceConfig {
  return isRecord(value) && typeof value.name === 'string' &&
    arrayOf(value.enabled_versions, (version) => ['h1', 'h2', 'h3'].includes(String(version))) &&
    typeof value.allow_absolute_form === 'boolean' && typeof value.tls_required === 'boolean' &&
    isRecord(value.connect) && typeof value.connect.enabled === 'boolean' &&
    arrayOf(value.connect.allowed_ports, safeInteger) &&
    isRecord(value.connect_udp) && typeof value.connect_udp.enabled === 'boolean' &&
    arrayOf(value.connect_udp.allowed_ports, safeInteger) &&
    isForwardPeerPolicy(value) &&
    (value.auth === null || isForwardProxyAuth(value.auth)) &&
    (value.access_policy === null || isForwardAccessPolicy(value.access_policy)) &&
    isRecord(value.destination_policy) &&
    arrayOf(value.destination_policy.allow_domains, isString) &&
    arrayOf(value.destination_policy.deny_domains, isString) &&
    arrayOf(value.destination_policy.allow_cidrs, isString) &&
    arrayOf(value.destination_policy.deny_cidrs, isString) &&
    typeof value.destination_policy.deny_private === 'boolean' &&
    arrayOf(value.destination_policy.allow_times, isForwardTimeRange) &&
    arrayOf(value.destination_policy.deny_times, isForwardTimeRange) &&
    isRecord(value.header_policy) && ['preserve', 'delete'].includes(String(value.header_policy.forwarded_for)) &&
    ['preserve', 'delete'].includes(String(value.header_policy.via)) &&
    (value.cache === undefined || value.cache === null || isHttpCachePolicy(value.cache)) &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.idle_timeout_ms) &&
    safeInteger(value.lifetime_timeout_ms) && nullableSafeInteger(value.max_request_body_bytes) &&
    safeInteger(value.max_header_bytes) && safeInteger(value.max_connections) &&
    isRecord(value.resolver) && arrayOf(value.resolver.nameservers, isString) &&
    safeInteger(value.resolver.max_cache_entries) &&
    safeInteger(value.resolver.max_concurrent_queries) &&
    safeInteger(value.resolver.max_addresses_per_name) && safeInteger(value.resolver.min_ttl_ms) &&
    safeInteger(value.resolver.max_ttl_ms) && safeInteger(value.resolver.negative_ttl_ms) &&
    typeof value.resolver.revalidate_on_connect === 'boolean' &&
    ['off', 'metadata'].includes(String(value.audit_mode))
}

function isForwardProxyAuth(value: unknown): value is ForwardProxyAuthConfig {
  return isRecord(value) && ((value.type === 'bearer_token_file' &&
    typeof value.token_file_path === 'string') || (value.type === 'basic_htpasswd_file' &&
    typeof value.htpasswd_file_path === 'string' && typeof value.realm === 'string' &&
    nullableSafeInteger(value.credential_ttl_ms) && typeof value.username_case_sensitive === 'boolean') ||
    (value.type === 'mutual_tls' && typeof value.client_ca_file_path === 'string'))
}

function isForwardPeerPolicy(value: unknown): boolean {
  if (!isRecord(value) || !isRecord(value.peer_policy)) return false
  const policy = value.peer_policy
  return ['allowed', 'denied', 'required'].includes(String(policy.direct_fallback)) &&
    integerInRange(policy.max_retries, 0, 15) &&
    Array.isArray(policy.peers) && policy.peers.length <= 16 &&
    policy.peers.every((peer: unknown) => isRecord(peer) && typeof peer.host === 'string' &&
      integerInRange(peer.port, 1, 65_535))
}

function isForwardAccessPolicy(value: unknown): value is ForwardAccessPolicyConfig {
  return isRecord(value) && ['allow', 'deny'].includes(String(value.default_action)) &&
    arrayOf(value.rules, (rule) => isRecord(rule) && ['allow', 'deny'].includes(String(rule.action)) &&
      arrayOf(rule.conditions, (condition) => isRecord(condition) && typeof condition.negated === 'boolean' &&
        isForwardAccessMatcher(condition)))
}

function isForwardAccessMatcher(value: unknown): value is ForwardAccessMatcherConfig {
  if (!isRecord(value)) return false
  if (['all', 'authenticated', 'destination_local', 'destination_link_local', 'manager'].includes(String(value.type))) return true
  if (value.type === 'methods') return arrayOf(value.methods, isString)
  if (value.type === 'source_cidrs') return arrayOf(value.cidrs, isString)
  return value.type === 'destination_ports' && arrayOf(value.ranges, (range) => isRecord(range) &&
    safeInteger(range.start) && safeInteger(range.end))
}

function isForwardTimeRange(value: unknown): value is ForwardTimeRangeConfig {
  return isRecord(value) && arrayOf(value.days, (day) =>
    ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'].includes(String(day))) &&
    typeof value.start === 'string' && typeof value.end === 'string'
}

function isRtmpRecorder(value: unknown): value is RtmpRecorderConfig {
  return isRecord(value) && typeof value.name === 'string' &&
    ['continuous', 'manual'].includes(String(value.start)) && typeof value.root_directory === 'string' &&
    isRecord(value.record_mask) && typeof value.record_mask.audio === 'boolean' &&
    typeof value.record_mask.video === 'boolean' && typeof value.record_mask.keyframes === 'boolean' &&
    typeof value.suffix_template === 'string' && typeof value.append_unix_seconds === 'boolean' &&
    typeof value.append === 'boolean' && typeof value.lock === 'boolean' && nullableSafeInteger(value.max_size) &&
    nullableSafeInteger(value.max_frames) && typeof value.notify === 'boolean' &&
    typeof value.timezone === 'string' && value.timezone.length > 0 &&
    ['segment_start', 'segment_end'].includes(String(value.time_basis)) &&
    ['safe_unique', 'nginx_compatible'].includes(String(value.segment_naming)) &&
    nullableSafeInteger(value.rotation_interval_ms) && safeInteger(value.max_queue_messages) &&
    safeInteger(value.max_queue_bytes) && safeInteger(value.shutdown_timeout_ms) &&
    nullableSafeInteger(value.max_storage_bytes) && nullableSafeInteger(value.max_storage_files) &&
    safeInteger(value.max_active_recorders)
}

function isL4Service(value: unknown): value is L4ServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && typeof value.upstream_pool === 'string' &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.idle_timeout_ms) &&
    nullableSafeInteger(value.lifetime_timeout_ms) &&
    (value.proxy_protocol === undefined || value.proxy_protocol === null || isProxyProtocolPolicy(value.proxy_protocol)) &&
    (value.udp === null || isUdpPolicy(value.udp))
}

function isUdpPolicy(value: unknown): value is UdpPolicyConfig {
  return isRecord(value) && integerInRange(value.max_datagram_bytes, 1, 65_507) &&
    integerInRange(value.max_sessions, 1, 100_000) &&
    integerInRange(value.max_session_bytes, 1, 1_073_741_824) &&
    integerInRange(value.max_queue_datagrams, 1, 4_096) &&
    integerInRange(value.max_queue_bytes, 1, 16_777_216)
}
