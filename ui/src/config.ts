import type { CandidateTopologySnapshot } from './api'
import {
  arrayOf,
  integerInRange,
  isRecord,
  isString,
  nullableSafeInteger,
  nullableString,
  safeInteger,
} from './valueGuards'

export type ListenerProtocol =
  | 'http'
  | 'tcp'
  | 'rtmp'
  | 'forward_http1'
  | 'forward_http2'
  | 'forward_http3'
export type TlsVersion = '1.2' | '1.3'
export type AlpnProtocol = 'h3' | 'h2' | 'http/1.1'
export type HttpVersion = '1.1' | '2'
export type HealthCheckType = 'http' | 'tcp'
export type UpstreamAlgorithm = 'round_robin' | 'least_connections'
export type RtmpRecorderStart = 'continuous' | 'manual'

export interface ManagementConfig {
  bind: string
  ui_dir: string | null
}

export interface DirectCertificateSource {
  type: 'files'
  certificate_chain_path: string
  private_key_path: string
}

export interface CertbotCertificateSource {
  type: 'certbot'
  live_directory_path: string
  archive_directory_path: string
}

export type CertificateSource = DirectCertificateSource | CertbotCertificateSource

export interface CertificateConfig {
  name: string
  dns_names: string[]
  source: CertificateSource
}

export interface TlsProfileConfig {
  name: string
  certificates: string[]
  default_certificate: string
  min_version: TlsVersion
  alpn: AlpnProtocol[]
}

export interface SocketListenerBind {
  type: 'socket'
  address: string
}

export interface UnixListenerBind {
  type: 'unix'
  path: string
}

export interface UdpListenerBind {
  type: 'udp'
  address: string
}

export type ListenerBind = SocketListenerBind | UdpListenerBind | UnixListenerBind

export interface ListenerConfig {
  name: string
  bind: ListenerBind
  protocol: ListenerProtocol
  service: string | null
  tls_profile: string | null
  max_connections: number | null
}

interface CacheStoreLimitsConfig {
  name: string
  max_bytes: number
  max_object_bytes: number
  max_header_bytes: number
  max_key_bytes: number
  max_tag_bytes: number
  max_tags_per_object: number
  max_in_flight_fills: number
  max_followers_per_fill: number
}

export interface MemoryCacheStoreConfig extends CacheStoreLimitsConfig {
  type: 'memory'
  max_entries: number
}

export interface DiskCacheStoreConfig extends CacheStoreLimitsConfig {
  type: 'disk'
  root_directory: string
  max_files: number
}

export type CacheStoreConfig = MemoryCacheStoreConfig | DiskCacheStoreConfig

export interface HealthCheckConfig {
  type: HealthCheckType
  interval_ms: number
  timeout_ms: number
  healthy_threshold: number
  unhealthy_threshold: number
  host: string | null
  path: string | null
}

export interface UpstreamTlsConfig {
  server_name: string
  ca_certificate_path: string | null
}

export interface HttpVersionPolicyConfig {
  min: HttpVersion
  max: HttpVersion
}

export interface SocketUpstreamEndpoint {
  type: 'socket'
  address: string
}

export interface DnsUpstreamEndpoint {
  type: 'dns'
  host: string
  port: number
}

export interface UnixUpstreamEndpoint {
  type: 'unix'
  path: string
}

export type UpstreamEndpoint =
  | SocketUpstreamEndpoint
  | DnsUpstreamEndpoint
  | UnixUpstreamEndpoint

export interface UpstreamPoolConfig {
  name: string
  endpoints: UpstreamEndpoint[]
  algorithm: UpstreamAlgorithm
  health_check: HealthCheckConfig | null
  tls: UpstreamTlsConfig | null
  http_versions: HttpVersionPolicyConfig
}

export type HttpHostKind = 'normalized_host' | 'exact_authority'
export type HttpPathKind = 'segment_prefix' | 'raw_prefix' | 'exact'
export const HTTP_RETRY_TRIGGERS = [
  'connect_failure',
  'connect_timeout',
  'refused_stream',
] as const
export type HttpRetryTrigger = typeof HTTP_RETRY_TRIGGERS[number]

export interface HttpHostSelectorConfig {
  kind: HttpHostKind
  value: string
}

export interface HttpPathSelectorConfig {
  kind: HttpPathKind
  value: string
}

export interface HttpBearerTokenFileAccessConfig {
  type: 'bearer_token_file'
  token_file_path: string
  header_name: string
  realm: string | null
}

export type HttpAccessPolicyConfig = HttpBearerTokenFileAccessConfig

export type HttpUpstreamHostConfig =
  | { type: 'preserve_incoming' }
  | { type: 'endpoint'; unix_fallback: string | null }
  | { type: 'literal'; value: string }

export type HttpRequestHeaderValueConfig =
  | { type: 'literal'; value: string }
  | { type: 'incoming_authority' }
  | { type: 'normalized_host' }
  | { type: 'client_ip' }
  | { type: 'selected_upstream_host' }

export type HttpRequestHeaderMutationConfig =
  | { operation: 'set'; name: string; value: HttpRequestHeaderValueConfig }
  | { operation: 'remove'; name: string }

export type HttpResponseHeaderMutationConfig =
  | { operation: 'set'; name: string; value: string }
  | { operation: 'remove'; name: string }

export interface HttpCookiePathRewriteConfig {
  from: string
  to: string
}

export interface HttpRetryPolicyConfig {
  max_retries: number
  triggers: HttpRetryTrigger[]
  method_safety: 'get_head'
  body_safety: 'empty'
}

export interface HttpProxyPolicyConfig {
  upstream_host: HttpUpstreamHostConfig
  request_headers: HttpRequestHeaderMutationConfig[]
  response_headers: HttpResponseHeaderMutationConfig[]
  response_cookie_path_rewrites: HttpCookiePathRewriteConfig[]
  retry: HttpRetryPolicyConfig
  cache: HttpCachePolicyConfig | null
}

export type CacheKeyComponentConfig =
  | { type: 'scheme' }
  | { type: 'normalized_host' }
  | { type: 'path_and_query' }
  | { type: 'header'; name: string }
  | { type: 'cookie'; name: string }

export interface CacheStatusTtlConfig {
  status: number
  ttl_ms: number
}

export type CacheStaleTrigger =
  | 'connect_failure'
  | 'connect_timeout'
  | 'origin_500'
  | 'origin_502'
  | 'origin_503'
  | 'origin_504'

export type CachePredicateConfig =
  | { type: 'header_present'; name: string }
  | { type: 'cookie_present'; name: string }

export interface CacheSurrogateTagsConfig {
  response_header: string
  max_tags: number
  max_tag_bytes: number
}

export interface CachePurgeAuthorizationConfig {
  type: 'bearer_token_file'
  token_file_path: string
}

export interface HttpCachePolicyConfig {
  store: string
  methods: string[]
  key_components: CacheKeyComponentConfig[]
  use_origin_cache_control: boolean
  default_ttl_ms: number
  status_ttls: CacheStatusTtlConfig[]
  grace_ms: number
  keep_ms: number
  revalidate: boolean
  collapsed_forwarding: boolean
  stale_on: CacheStaleTrigger[]
  bypass_request: CachePredicateConfig[]
  no_store_request: CachePredicateConfig[]
  no_store_response: CachePredicateConfig[]
  set_cookie_policy: 'bypass' | 'ignore'
  authorization_policy: 'bypass' | 'cache'
  vary_policy: 'respect' | 'ignore'
  surrogate_tags: CacheSurrogateTagsConfig | null
  purge_authorization: CachePurgeAuthorizationConfig | null
}

export interface HttpLiteralHeaderConfig {
  name: string
  value: string
}

export type HttpRedirectLocationConfig =
  | { kind: 'literal'; value: string }
  | { kind: 'request_template'; value: string }

export interface HttpProxyActionConfig {
  type: 'proxy'
  upstream_pool: string
  policy: HttpProxyPolicyConfig
}

export interface HttpFixedResponseActionConfig {
  type: 'fixed_response'
  status: number
  body: string
  headers: HttpLiteralHeaderConfig[]
}

export interface HttpRedirectActionConfig {
  type: 'redirect'
  status: number
  location: HttpRedirectLocationConfig
}

export interface HttpStaticFilesActionConfig {
  type: 'static_files'
  root_directory: string
  index_files: string[]
  spa_fallback: string | null
}

export type HttpRouteActionConfig =
  | HttpProxyActionConfig
  | HttpFixedResponseActionConfig
  | HttpRedirectActionConfig
  | HttpStaticFilesActionConfig

export interface HttpRouteConfig {
  host: HttpHostSelectorConfig | null
  path: HttpPathSelectorConfig
  methods: string[]
  access_policy: HttpAccessPolicyConfig | null
  action: HttpRouteActionConfig
}

export interface HttpServiceConfig {
  name: string
  routes: HttpRouteConfig[]
  upstream_io_timeout_ms: number
  max_request_body_bytes: number | null
}

export interface RtmpApplicationConfig {
  name: string
  live: boolean
  idle_streams: boolean
  recorders: RtmpRecorderConfig[]
}

export interface RtmpRecorderConfig {
  name: string
  start: RtmpRecorderStart
  root_directory: string
  suffix_template: string
  append_unix_seconds: boolean
  rotation_interval_ms: number | null
  max_queue_messages: number
  max_queue_bytes: number
  shutdown_timeout_ms: number
  max_storage_bytes: number
  max_storage_files: number
  max_active_recorders: number
}

export interface RtmpServiceConfig {
  name: string
  applications: RtmpApplicationConfig[]
}

export type ForwardHttpVersion = 'h1' | 'h2' | 'h3'

export interface ForwardConnectPolicyConfig {
  enabled: boolean
  allowed_ports: number[]
}

export interface ForwardProxyAuthConfig {
  type: 'bearer_token_file'
  token_file_path: string
}

export interface ForwardDestinationPolicyConfig {
  allow_domains: string[]
  deny_domains: string[]
  allow_cidrs: string[]
  deny_cidrs: string[]
  deny_private: boolean
}

export interface ForwardResolverPolicyConfig {
  max_cache_entries: number
  max_concurrent_queries: number
  max_addresses_per_name: number
  min_ttl_ms: number
  max_ttl_ms: number
  negative_ttl_ms: number
  revalidate_on_connect: boolean
}

export interface ForwardProxyServiceConfig {
  name: string
  enabled_versions: ForwardHttpVersion[]
  allow_absolute_form: boolean
  tls_required: boolean
  connect: ForwardConnectPolicyConfig
  auth: ForwardProxyAuthConfig | null
  destination_policy: ForwardDestinationPolicyConfig
  connect_timeout_ms: number
  idle_timeout_ms: number
  lifetime_timeout_ms: number
  max_request_body_bytes: number | null
  max_header_bytes: number
  max_connections: number
  resolver: ForwardResolverPolicyConfig
  audit_mode: 'off' | 'metadata'
}

export interface L4ServiceConfig {
  name: string
  upstream_pool: string
  connect_timeout_ms: number
  idle_timeout_ms: number
  lifetime_timeout_ms: number | null
}

export interface CanonicalConfig {
  version: number
  management: ManagementConfig | null
  certificates: CertificateConfig[]
  tls_profiles: TlsProfileConfig[]
  listeners: ListenerConfig[]
  cache_stores: CacheStoreConfig[]
  upstream_pools: UpstreamPoolConfig[]
  http_services: HttpServiceConfig[]
  forward_proxy_services: ForwardProxyServiceConfig[]
  rtmp_services: RtmpServiceConfig[]
  l4_services: L4ServiceConfig[]
}

export type DiagnosticSeverity = 'error' | 'warning'
export type DiagnosticStage =
  | 'read'
  | 'parse'
  | 'validation'
  | 'render'
  | 'conflict'
  | 'write'
  | 'sync'
  | 'rollback'
  | 'activation'

export interface ConfigDiagnostic {
  code: string
  severity: DiagnosticSeverity
  stage: DiagnosticStage
  message: string
  path?: string
}

export interface ConfigSnapshot {
  schemaVersion: 1
  diskRevision: string
  activeRevision: string | null
  config: CanonicalConfig
  diagnostics: ConfigDiagnostic[]
}

export interface ConfigValidationResponse {
  candidateRevision: string
  normalizedConfig: CanonicalConfig
  luaPreview: string
  diagnostics: ConfigDiagnostic[]
  topology: CandidateTopologySnapshot
}

export type ConfigSaveOutcome = 'saved_restart_required' | 'unchanged_active'
export type ConfigActivationState = 'restart_required' | 'active'

export interface ConfigSaveResponse {
  diskRevision: string
  activeRevision: string | null
  outcome: ConfigSaveOutcome
  activationState: ConfigActivationState
  restartRequired: boolean
  diagnostics: ConfigDiagnostic[]
}

export interface ConfigRequest {
  config: CanonicalConfig
}

export function isCanonicalConfig(value: unknown): value is CanonicalConfig {
  if (!isRecord(value) || !safeInteger(value.version) ||
    !(value.management === null || (isRecord(value.management) &&
      typeof value.management.bind === 'string' && nullableString(value.management.ui_dir))) ||
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
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.dns_names, isString) &&
    isRecord(value.source) && (value.source.type === 'files'
      ? typeof value.source.certificate_chain_path === 'string' && typeof value.source.private_key_path === 'string'
      : value.source.type === 'certbot' && typeof value.source.live_directory_path === 'string' &&
        typeof value.source.archive_directory_path === 'string')
}

function isTlsProfile(value: unknown): value is TlsProfileConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.certificates, isString) &&
    typeof value.default_certificate === 'string' && ['1.2', '1.3'].includes(String(value.min_version)) &&
    arrayOf(value.alpn, (entry) => ['h3', 'h2', 'http/1.1'].includes(String(entry)))
}

function isListener(value: unknown): value is ListenerConfig {
  return isRecord(value) && typeof value.name === 'string' && isListenerBind(value.bind) &&
    ['http', 'tcp', 'rtmp', 'forward_http1', 'forward_http2', 'forward_http3']
      .includes(String(value.protocol)) && nullableString(value.service) &&
    nullableString(value.tls_profile) && nullableSafeInteger(value.max_connections)
}

function isListenerBind(value: unknown): value is ListenerBind {
  return isRecord(value) && (value.type === 'socket' || value.type === 'udp'
    ? typeof value.address === 'string'
    : value.type === 'unix' && typeof value.path === 'string')
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

function isUpstreamPool(value: unknown): value is UpstreamPoolConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.endpoints, isEndpoint) &&
    ['round_robin', 'least_connections'].includes(String(value.algorithm)) &&
    (value.health_check === null || isHealthCheck(value.health_check)) &&
    (value.tls === null || (isRecord(value.tls) && typeof value.tls.server_name === 'string' &&
      nullableString(value.tls.ca_certificate_path))) && isRecord(value.http_versions) &&
    ['1.1', '2'].includes(String(value.http_versions.min)) &&
    ['1.1', '2'].includes(String(value.http_versions.max))
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
    nullableString(value.host) && nullableString(value.path)
}

function isHttpService(value: unknown): value is HttpServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.routes, (route) =>
    isRecord(route) && (route.host === null || isHttpHost(route.host)) && isHttpPath(route.path) &&
    arrayOf(route.methods, isString) && (route.access_policy === null || isHttpAccess(route.access_policy)) &&
    isHttpAction(route.action)) && safeInteger(value.upstream_io_timeout_ms) &&
    nullableSafeInteger(value.max_request_body_bytes)
}

function isHttpHost(value: unknown): value is HttpHostSelectorConfig {
  return isRecord(value) && ['normalized_host', 'exact_authority'].includes(String(value.kind)) &&
    typeof value.value === 'string'
}

function isHttpPath(value: unknown): value is HttpPathSelectorConfig {
  return isRecord(value) && ['segment_prefix', 'raw_prefix', 'exact'].includes(String(value.kind)) &&
    typeof value.value === 'string'
}

function isHttpAccess(value: unknown): value is HttpAccessPolicyConfig {
  return isRecord(value) && value.type === 'bearer_token_file' &&
    typeof value.token_file_path === 'string' && typeof value.header_name === 'string' &&
    nullableString(value.realm)
}

function isHttpAction(value: unknown): value is HttpRouteActionConfig {
  if (!isRecord(value)) return false
  switch (value.type) {
    case 'proxy':
      return typeof value.upstream_pool === 'string' && isHttpProxyPolicy(value.policy)
    case 'fixed_response':
      return integerInRange(value.status, 200, 599) && typeof value.body === 'string' &&
        arrayOf(value.headers, (header) => isRecord(header) &&
          typeof header.name === 'string' && typeof header.value === 'string')
    case 'redirect':
      return [301, 302, 307, 308].includes(Number(value.status)) && isRecord(value.location) &&
        ['literal', 'request_template'].includes(String(value.location.kind)) &&
        typeof value.location.value === 'string'
    case 'static_files':
      return typeof value.root_directory === 'string' && arrayOf(value.index_files, isString) &&
        nullableString(value.spa_fallback)
    default:
      return false
  }
}

function isHttpProxyPolicy(value: unknown): value is HttpProxyPolicyConfig {
  return isRecord(value) && isHttpUpstreamHost(value.upstream_host) &&
    arrayOf(value.request_headers, isRequestHeaderMutation) &&
    arrayOf(value.response_headers, isResponseHeaderMutation) &&
    arrayOf(value.response_cookie_path_rewrites, (rewrite) => isRecord(rewrite) &&
      typeof rewrite.from === 'string' && typeof rewrite.to === 'string') &&
    isRecord(value.retry) && integerInRange(value.retry.max_retries, 0, 2) &&
    isHttpRetryTriggers(value.retry.triggers) &&
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

function isHttpRetryTriggers(value: unknown): value is HttpRetryTrigger[] {
  return Array.isArray(value) && value.length > 0 &&
    value.every((trigger) => HTTP_RETRY_TRIGGERS.includes(trigger as HttpRetryTrigger)) &&
    new Set(value).size === value.length
}

function isHttpUpstreamHost(value: unknown): value is HttpUpstreamHostConfig {
  return isRecord(value) && (value.type === 'preserve_incoming' ||
    (value.type === 'endpoint' && nullableString(value.unix_fallback)) ||
    (value.type === 'literal' && typeof value.value === 'string'))
}

function isRequestHeaderMutation(value: unknown): value is HttpRequestHeaderMutationConfig {
  return isRecord(value) && typeof value.name === 'string' && (value.operation === 'remove' ||
    (value.operation === 'set' && isRecord(value.value) &&
      ['literal', 'incoming_authority', 'normalized_host', 'client_ip', 'selected_upstream_host']
        .includes(String(value.value.type)) &&
      (value.value.type !== 'literal' || typeof value.value.value === 'string')))
}

function isResponseHeaderMutation(value: unknown): value is HttpResponseHeaderMutationConfig {
  return isRecord(value) && typeof value.name === 'string' && (value.operation === 'remove' ||
    (value.operation === 'set' && typeof value.value === 'string'))
}

function isRtmpService(value: unknown): value is RtmpServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.applications, (application) =>
    isRecord(application) && typeof application.name === 'string' && typeof application.live === 'boolean' &&
    typeof application.idle_streams === 'boolean' && arrayOf(application.recorders, isRtmpRecorder))
}

function isForwardProxyService(value: unknown): value is ForwardProxyServiceConfig {
  return isRecord(value) && typeof value.name === 'string' &&
    arrayOf(value.enabled_versions, (version) => ['h1', 'h2', 'h3'].includes(String(version))) &&
    typeof value.allow_absolute_form === 'boolean' && typeof value.tls_required === 'boolean' &&
    isRecord(value.connect) && typeof value.connect.enabled === 'boolean' &&
    arrayOf(value.connect.allowed_ports, safeInteger) &&
    (value.auth === null || (isRecord(value.auth) && value.auth.type === 'bearer_token_file' &&
      typeof value.auth.token_file_path === 'string')) && isRecord(value.destination_policy) &&
    arrayOf(value.destination_policy.allow_domains, isString) &&
    arrayOf(value.destination_policy.deny_domains, isString) &&
    arrayOf(value.destination_policy.allow_cidrs, isString) &&
    arrayOf(value.destination_policy.deny_cidrs, isString) &&
    typeof value.destination_policy.deny_private === 'boolean' &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.idle_timeout_ms) &&
    safeInteger(value.lifetime_timeout_ms) && nullableSafeInteger(value.max_request_body_bytes) &&
    safeInteger(value.max_header_bytes) && safeInteger(value.max_connections) &&
    isRecord(value.resolver) && safeInteger(value.resolver.max_cache_entries) &&
    safeInteger(value.resolver.max_concurrent_queries) &&
    safeInteger(value.resolver.max_addresses_per_name) && safeInteger(value.resolver.min_ttl_ms) &&
    safeInteger(value.resolver.max_ttl_ms) && safeInteger(value.resolver.negative_ttl_ms) &&
    typeof value.resolver.revalidate_on_connect === 'boolean' &&
    ['off', 'metadata'].includes(String(value.audit_mode))
}

function isRtmpRecorder(value: unknown): value is RtmpRecorderConfig {
  return isRecord(value) && typeof value.name === 'string' &&
    ['continuous', 'manual'].includes(String(value.start)) && typeof value.root_directory === 'string' &&
    typeof value.suffix_template === 'string' && typeof value.append_unix_seconds === 'boolean' &&
    nullableSafeInteger(value.rotation_interval_ms) && safeInteger(value.max_queue_messages) &&
    safeInteger(value.max_queue_bytes) && safeInteger(value.shutdown_timeout_ms) &&
    safeInteger(value.max_storage_bytes) && safeInteger(value.max_storage_files) &&
    safeInteger(value.max_active_recorders)
}

function isL4Service(value: unknown): value is L4ServiceConfig {
  return isRecord(value) && typeof value.name === 'string' && typeof value.upstream_pool === 'string' &&
    safeInteger(value.connect_timeout_ms) && safeInteger(value.idle_timeout_ms) &&
    nullableSafeInteger(value.lifetime_timeout_ms)
}

export type CanonicalFieldKind =
  | 'boolean'
  | 'collection'
  | 'enum'
  | 'integer'
  | 'object'
  | 'reference'
  | 'string'
  | 'string_list'

export interface CanonicalFieldDefinition {
  path: string
  kind: CanonicalFieldKind
}

// Keep this inventory in schema order. Its exact-set test makes a schema expansion fail visibly.
export const CANONICAL_FIELD_REGISTRY = [
  { path: 'version', kind: 'integer' },
  { path: 'management', kind: 'object' },
  { path: 'management.bind', kind: 'string' },
  { path: 'management.ui_dir', kind: 'string' },
  { path: 'certificates', kind: 'collection' },
  { path: 'certificates[].name', kind: 'string' },
  { path: 'certificates[].dns_names', kind: 'string_list' },
  { path: 'certificates[].source', kind: 'object' },
  { path: 'certificates[].source.type', kind: 'enum' },
  { path: 'certificates[].source.certificate_chain_path', kind: 'string' },
  { path: 'certificates[].source.private_key_path', kind: 'string' },
  { path: 'certificates[].source.live_directory_path', kind: 'string' },
  { path: 'certificates[].source.archive_directory_path', kind: 'string' },
  { path: 'tls_profiles', kind: 'collection' },
  { path: 'tls_profiles[].name', kind: 'string' },
  { path: 'tls_profiles[].certificates', kind: 'string_list' },
  { path: 'tls_profiles[].default_certificate', kind: 'reference' },
  { path: 'tls_profiles[].min_version', kind: 'enum' },
  { path: 'tls_profiles[].alpn', kind: 'enum' },
  { path: 'listeners', kind: 'collection' },
  { path: 'listeners[].name', kind: 'string' },
  { path: 'listeners[].bind', kind: 'object' },
  { path: 'listeners[].bind.type', kind: 'enum' },
  { path: 'listeners[].bind.address', kind: 'string' },
  { path: 'listeners[].bind.path', kind: 'string' },
  { path: 'listeners[].protocol', kind: 'enum' },
  { path: 'listeners[].service', kind: 'reference' },
  { path: 'listeners[].tls_profile', kind: 'reference' },
  { path: 'listeners[].max_connections', kind: 'integer' },
  { path: 'cache_stores', kind: 'collection' },
  { path: 'cache_stores[].type', kind: 'enum' },
  { path: 'cache_stores[].name', kind: 'string' },
  { path: 'cache_stores[].root_directory', kind: 'string' },
  { path: 'cache_stores[].max_bytes', kind: 'integer' },
  { path: 'cache_stores[].max_entries', kind: 'integer' },
  { path: 'cache_stores[].max_files', kind: 'integer' },
  { path: 'cache_stores[].max_object_bytes', kind: 'integer' },
  { path: 'cache_stores[].max_header_bytes', kind: 'integer' },
  { path: 'cache_stores[].max_key_bytes', kind: 'integer' },
  { path: 'cache_stores[].max_tag_bytes', kind: 'integer' },
  { path: 'cache_stores[].max_tags_per_object', kind: 'integer' },
  { path: 'cache_stores[].max_in_flight_fills', kind: 'integer' },
  { path: 'cache_stores[].max_followers_per_fill', kind: 'integer' },
  { path: 'upstream_pools', kind: 'collection' },
  { path: 'upstream_pools[].name', kind: 'string' },
  { path: 'upstream_pools[].endpoints', kind: 'collection' },
  { path: 'upstream_pools[].endpoints[].type', kind: 'enum' },
  { path: 'upstream_pools[].endpoints[].address', kind: 'string' },
  { path: 'upstream_pools[].endpoints[].host', kind: 'string' },
  { path: 'upstream_pools[].endpoints[].port', kind: 'integer' },
  { path: 'upstream_pools[].endpoints[].path', kind: 'string' },
  { path: 'upstream_pools[].algorithm', kind: 'enum' },
  { path: 'upstream_pools[].health_check', kind: 'object' },
  { path: 'upstream_pools[].health_check.type', kind: 'enum' },
  { path: 'upstream_pools[].health_check.interval_ms', kind: 'integer' },
  { path: 'upstream_pools[].health_check.timeout_ms', kind: 'integer' },
  { path: 'upstream_pools[].health_check.healthy_threshold', kind: 'integer' },
  { path: 'upstream_pools[].health_check.unhealthy_threshold', kind: 'integer' },
  { path: 'upstream_pools[].health_check.host', kind: 'string' },
  { path: 'upstream_pools[].health_check.path', kind: 'string' },
  { path: 'upstream_pools[].tls', kind: 'object' },
  { path: 'upstream_pools[].tls.server_name', kind: 'string' },
  { path: 'upstream_pools[].tls.ca_certificate_path', kind: 'string' },
  { path: 'upstream_pools[].http_versions', kind: 'object' },
  { path: 'upstream_pools[].http_versions.min', kind: 'enum' },
  { path: 'upstream_pools[].http_versions.max', kind: 'enum' },
  { path: 'http_services', kind: 'collection' },
  { path: 'http_services[].name', kind: 'string' },
  { path: 'http_services[].routes', kind: 'collection' },
  { path: 'http_services[].routes[].host', kind: 'object' },
  { path: 'http_services[].routes[].host.kind', kind: 'enum' },
  { path: 'http_services[].routes[].host.value', kind: 'string' },
  { path: 'http_services[].routes[].path', kind: 'object' },
  { path: 'http_services[].routes[].path.kind', kind: 'enum' },
  { path: 'http_services[].routes[].path.value', kind: 'string' },
  { path: 'http_services[].routes[].methods', kind: 'string_list' },
  { path: 'http_services[].routes[].access_policy', kind: 'object' },
  { path: 'http_services[].routes[].access_policy.type', kind: 'enum' },
  { path: 'http_services[].routes[].access_policy.token_file_path', kind: 'string' },
  { path: 'http_services[].routes[].access_policy.header_name', kind: 'string' },
  { path: 'http_services[].routes[].access_policy.realm', kind: 'string' },
  { path: 'http_services[].routes[].action', kind: 'object' },
  { path: 'http_services[].routes[].action.type', kind: 'enum' },
  { path: 'http_services[].routes[].action.upstream_pool', kind: 'reference' },
  { path: 'http_services[].routes[].action.policy', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.upstream_host', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.upstream_host.type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.upstream_host.unix_fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.upstream_host.value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.request_headers[].operation', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.request_headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.response_headers[].operation', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.response_headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_headers[].value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites[].from', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites[].to', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.retry', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.retry.max_retries', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.retry.triggers', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.retry.method_safety', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.retry.body_safety', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.cache.store', kind: 'reference' },
  { path: 'http_services[].routes[].action.policy.cache.methods', kind: 'string_list' },
  { path: 'http_services[].routes[].action.policy.cache.key_components', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.cache.key_components[].type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.key_components[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.cache.use_origin_cache_control', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.cache.default_ttl_ms', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.status_ttls', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.cache.status_ttls[].status', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.status_ttls[].ttl_ms', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.grace_ms', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.keep_ms', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.revalidate', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.cache.collapsed_forwarding', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.cache.stale_on', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.bypass_request', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.cache.bypass_request[].type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.bypass_request[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_request', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_request[].type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_request[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_response', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_response[].type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.no_store_response[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.cache.set_cookie_policy', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.authorization_policy', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.vary_policy', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.surrogate_tags', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.cache.surrogate_tags.response_header', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.cache.surrogate_tags.max_tags', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.surrogate_tags.max_tag_bytes', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.cache.purge_authorization', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.cache.purge_authorization.type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.cache.purge_authorization.token_file_path', kind: 'string' },
  { path: 'http_services[].routes[].action.status', kind: 'integer' },
  { path: 'http_services[].routes[].action.body', kind: 'string' },
  { path: 'http_services[].routes[].action.headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.headers[].value', kind: 'string' },
  { path: 'http_services[].routes[].action.location', kind: 'object' },
  { path: 'http_services[].routes[].action.location.kind', kind: 'enum' },
  { path: 'http_services[].routes[].action.location.value', kind: 'string' },
  { path: 'http_services[].routes[].action.root_directory', kind: 'string' },
  { path: 'http_services[].routes[].action.index_files', kind: 'string_list' },
  { path: 'http_services[].routes[].action.spa_fallback', kind: 'string' },
  { path: 'http_services[].upstream_io_timeout_ms', kind: 'integer' },
  { path: 'http_services[].max_request_body_bytes', kind: 'integer' },
  { path: 'forward_proxy_services', kind: 'collection' },
  { path: 'forward_proxy_services[].name', kind: 'string' },
  { path: 'forward_proxy_services[].enabled_versions', kind: 'enum' },
  { path: 'forward_proxy_services[].allow_absolute_form', kind: 'boolean' },
  { path: 'forward_proxy_services[].tls_required', kind: 'boolean' },
  { path: 'forward_proxy_services[].connect', kind: 'object' },
  { path: 'forward_proxy_services[].connect.enabled', kind: 'boolean' },
  { path: 'forward_proxy_services[].connect.allowed_ports', kind: 'collection' },
  { path: 'forward_proxy_services[].auth', kind: 'object' },
  { path: 'forward_proxy_services[].auth.type', kind: 'enum' },
  { path: 'forward_proxy_services[].auth.token_file_path', kind: 'string' },
  { path: 'forward_proxy_services[].destination_policy', kind: 'object' },
  { path: 'forward_proxy_services[].destination_policy.allow_domains', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_domains', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.allow_cidrs', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_cidrs', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_private', kind: 'boolean' },
  { path: 'forward_proxy_services[].connect_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].idle_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].lifetime_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].max_request_body_bytes', kind: 'integer' },
  { path: 'forward_proxy_services[].max_header_bytes', kind: 'integer' },
  { path: 'forward_proxy_services[].max_connections', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver', kind: 'object' },
  { path: 'forward_proxy_services[].resolver.max_cache_entries', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.max_concurrent_queries', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.max_addresses_per_name', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.min_ttl_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.max_ttl_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.negative_ttl_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver.revalidate_on_connect', kind: 'boolean' },
  { path: 'forward_proxy_services[].audit_mode', kind: 'enum' },
  { path: 'rtmp_services', kind: 'collection' },
  { path: 'rtmp_services[].name', kind: 'string' },
  { path: 'rtmp_services[].applications', kind: 'collection' },
  { path: 'rtmp_services[].applications[].name', kind: 'string' },
  { path: 'rtmp_services[].applications[].live', kind: 'boolean' },
  { path: 'rtmp_services[].applications[].idle_streams', kind: 'boolean' },
  { path: 'rtmp_services[].applications[].recorders', kind: 'collection' },
  { path: 'rtmp_services[].applications[].recorders[].name', kind: 'string' },
  { path: 'rtmp_services[].applications[].recorders[].start', kind: 'enum' },
  { path: 'rtmp_services[].applications[].recorders[].root_directory', kind: 'string' },
  { path: 'rtmp_services[].applications[].recorders[].suffix_template', kind: 'string' },
  { path: 'rtmp_services[].applications[].recorders[].append_unix_seconds', kind: 'boolean' },
  { path: 'rtmp_services[].applications[].recorders[].rotation_interval_ms', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].max_queue_messages', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].max_queue_bytes', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].shutdown_timeout_ms', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].max_storage_bytes', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].max_storage_files', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].max_active_recorders', kind: 'integer' },
  { path: 'l4_services', kind: 'collection' },
  { path: 'l4_services[].name', kind: 'string' },
  { path: 'l4_services[].upstream_pool', kind: 'reference' },
  { path: 'l4_services[].connect_timeout_ms', kind: 'integer' },
  { path: 'l4_services[].idle_timeout_ms', kind: 'integer' },
  { path: 'l4_services[].lifetime_timeout_ms', kind: 'integer' },
] as const satisfies readonly CanonicalFieldDefinition[]
