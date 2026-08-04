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
export type UpstreamAlgorithm = 'round_robin' | 'least_connections' | 'first'
export type RtmpRecorderStart = 'continuous' | 'manual'
export type AccessLogConfig = { type: 'disabled' } | { type: 'file'; path: string }

export interface ManagementConfig {
  bind: string
  ui_dir: string | null
}

export interface StatsConfig {
  binds: string[]
  admin_token_file: string | null
  pages: StatsPageConfig[]
}

export interface StatsPageConfig {
  bind: string
  uri_prefix: string
  refresh_ms: number
  admin: 'disabled' | 'localhost'
  max_connections: number | null
  downstream_timeouts: {
    client_timeout_ms: number | null
    request_timeout_ms: number | null
    keepalive_timeout_ms: number | null
  }
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

export type AcmeChallengeType = 'http01' | 'dns01' | 'tls_alpn01'
export type AcmeKeyType = 'ecdsa_p256' | 'rsa_2048'

export interface AcmeManagedCertificateSource {
  type: 'acme_managed'
  directory_url: string
  state_root: string
  contacts: string[]
  terms_agreed: boolean
  challenge: AcmeChallengeType
  key_type: AcmeKeyType
  allowed_dns_suffixes: string[]
}

export type SelfSignedKeyType = 'ecdsa_p256' | 'rsa_2048'

export interface SelfSignedDevelopmentCertificateSource {
  type: 'self_signed_development'
  validity_days: number
  key_type: SelfSignedKeyType
}

export type CertificateSource =
  | DirectCertificateSource
  | CertbotCertificateSource
  | AcmeManagedCertificateSource
  | SelfSignedDevelopmentCertificateSource

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
  policy: TlsPolicyConfig
}

export interface TlsPolicyConfig {
  cipher_list: string | null
  dh_parameters_path: string | null
  session_cache: TlsSessionCacheConfig | null
  session_timeout_seconds: number | null
  session_tickets: boolean
  prefer_server_ciphers: boolean
}

export interface TlsSessionCacheConfig {
  name: string
  size_bytes: number
}

export interface SocketListenerBind {
  type: 'socket'
  address: string
}

export interface UnixListenerBind {
  type: 'unix'
  path: string
  mode: number | null
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
  downstream_timeouts: {
    client_timeout_ms: number | null
    request_timeout_ms: number | null
    keepalive_timeout_ms: number | null
  }
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
  startup: 'healthy' | 'unhealthy' | 'checking'
  fast_interval_ms: number | null
  down_interval_ms: number | null
  host: string | null
  path: string | null
  expected_status: number | null
  http_version: '1.0' | '1.1' | null
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

export interface UpstreamServerConfig {
  name: string
  endpoint: UpstreamEndpoint
  max_connections: number | null
  dns_resolution: 'startup' | 'on_connect'
}

export interface UpstreamPoolConfig {
  name: string
  servers: UpstreamServerConfig[]
  endpoints?: UpstreamEndpoint[]
  algorithm: UpstreamAlgorithm
  health_check: HealthCheckConfig | null
  tls: UpstreamTlsConfig | null
  http_versions: HttpVersionPolicyConfig
  queue_timeout_ms: number | null
  connect_timeout_ms: number | null
  server_timeout_ms: number | null
  connection_reuse: 'never' | 'safe' | 'always'
}

export type HttpHostKind =
  | 'normalized_host'
  | 'exact_authority'
  | 'ascii_case_insensitive_exact_authority'
  | 'nginx_leading_wildcard'
  | 'nginx_leading_dot'
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

export interface HttpBasicHtpasswdFileAccessConfig {
  type: 'basic_htpasswd_file'
  htpasswd_file_path: string
  realm: string
}

export type HttpAccessPolicyConfig =
  | HttpBearerTokenFileAccessConfig
  | HttpBasicHtpasswdFileAccessConfig

export type HttpUpstreamHostConfig =
  | { type: 'preserve_incoming' }
  | { type: 'nginx_host'; fallback: string }
  | { type: 'endpoint'; unix_fallback: string | null }
  | { type: 'literal'; value: string }

export type HttpRequestHeaderValueConfig =
  | { type: 'literal'; value: string }
  | { type: 'incoming_authority' }
  | { type: 'normalized_host' }
  | { type: 'nginx_host'; fallback: string }
  | { type: 'client_ip' }
  | { type: 'appended_x_forwarded_for'; max_bytes: number; except_source_cidrs: string[] }
  | { type: 'downstream_scheme' }
  | { type: 'incoming_header'; name: string; max_bytes: number }
  | { type: 'selected_upstream_host' }

export type HttpRequestHeaderMutationConfig =
  | { operation: 'set'; name: string; value: HttpRequestHeaderValueConfig }
  | { operation: 'remove'; name: string }

export type HttpResponseHeaderMutationConfig =
  | { operation: 'set'; name: string; value: string; always: boolean }
  | { operation: 'add'; name: string; value: string; always: boolean }
  | { operation: 'remove'; name: string }

export interface HttpCookiePathRewriteConfig {
  from: string
  to: string
}

export interface HttpCookieAttributePolicyConfig {
  name: string
  secure: boolean | null
  http_only: boolean | null
  same_site: 'strict' | 'lax' | 'none' | null
}

export interface HttpRetryPolicyConfig {
  max_retries: number
  target: 'same_server' | 'next_server'
  delay_ms: number
  final_redispatch: boolean
  triggers: HttpRetryTrigger[]
  method_safety: 'get_head'
  body_safety: 'empty'
}

export interface HttpProxyPolicyConfig {
  upstream_host: HttpUpstreamHostConfig
  request_headers: HttpRequestHeaderMutationConfig[]
  response_headers: HttpResponseHeaderMutationConfig[]
  response_cookie_path_rewrites: HttpCookiePathRewriteConfig[]
  response_cookie_attributes: HttpCookieAttributePolicyConfig[]
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
  always?: boolean
}

export type HttpRedirectLocationConfig =
  | { kind: 'literal'; value: string }
  | { kind: 'request_template'; value: string; nginx_host_fallback: string | null }

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
  headers: HttpLiteralHeaderConfig[]
}

export interface HttpStaticFilesActionConfig {
  type: 'static_files'
  root_directory: string
  path_mapping: 'root' | 'alias'
  index_files: string[]
  internal_index_redirects: boolean
  directory_redirects: boolean
  spa_fallback: string | null
  try_files: Array<
    | { type: 'request_path' }
    | { type: 'request_path_directory' }
    | { type: 'relative'; path: string }
    | { type: 'status'; status: number }
  >
  autoindex: boolean
  autoindex_exact_size: boolean
  autoindex_local_time: boolean
  etag: boolean
  mime: {
    default_type: string | null
    types: Array<{ extension: string; content_type: string }>
  }
  headers: HttpLiteralHeaderConfig[]
  error_responses: Array<{
    statuses: number[]
    file: string | null
    body: string | null
    headers: HttpLiteralHeaderConfig[]
    internal_redirect: string | null
  }>
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
  policy: {
    max_request_body_bytes: number | null
    connect_timeout_ms: number
    read_timeout_ms: number
    write_timeout_ms: number
    request_buffering: boolean
    response_buffering: boolean
  }
  action: HttpRouteActionConfig
}

export interface HttpServiceConfig {
  name: string
  routes: HttpRouteConfig[]
  automatic_response_headers: boolean
  upstream_io_timeout_ms: number
  max_request_body_bytes: number | null
  gzip: {
    level: number
    content_types: string[]
    min_length_bytes: number
    min_http_version: '1.0' | '1.1'
    disable_on_via: boolean
    vary: boolean
  } | null
  access_log: AccessLogConfig | null
}

export interface RtmpApplicationConfig {
  name: string
  live: boolean
  idle_streams: boolean
  push_targets: Array<{ host: string; port: number; application: string }>
  fanout: {
    max_subscribers: number
    max_queue_messages_per_subscriber: number
    max_queue_bytes_per_subscriber: number
  }
  recorders: RtmpRecorderConfig[]
}

export interface RtmpRecorderConfig {
  name: string
  start: RtmpRecorderStart
  root_directory: string
  suffix_template: string
  append_unix_seconds: boolean
  timezone: string
  time_basis: 'segment_start' | 'segment_end'
  segment_naming: 'safe_unique' | 'nginx_compatible'
  rotation_interval_ms: number | null
  max_queue_messages: number
  max_queue_bytes: number
  shutdown_timeout_ms: number
  max_storage_bytes: number | null
  max_storage_files: number | null
  max_active_recorders: number
}

export interface RtmpServiceConfig {
  name: string
  outbound_chunk_size: number
  access_log: AccessLogConfig | null
  applications: RtmpApplicationConfig[]
}

export type ForwardHttpVersion = 'h1' | 'h2' | 'h3'

export interface ForwardConnectPolicyConfig {
  enabled: boolean
  allowed_ports: number[]
}

export type ForwardProxyAuthConfig =
  | { type: 'bearer_token_file'; token_file_path: string }
  | {
      type: 'basic_htpasswd_file'
      htpasswd_file_path: string
      realm: string
      credential_ttl_ms: number | null
      username_case_sensitive: boolean
    }

export interface ForwardPortRangeConfig { start: number; end: number }

export type ForwardAccessMatcherConfig =
  | { type: 'all' }
  | { type: 'methods'; methods: string[] }
  | { type: 'source_cidrs'; cidrs: string[] }
  | { type: 'destination_ports'; ranges: ForwardPortRangeConfig[] }
  | { type: 'authenticated' }
  | { type: 'destination_local' }
  | { type: 'destination_link_local' }
  | { type: 'manager' }

export type ForwardAccessConditionConfig = ForwardAccessMatcherConfig & { negated: boolean }

export interface ForwardAccessPolicyConfig {
  rules: Array<{ action: 'allow' | 'deny'; conditions: ForwardAccessConditionConfig[] }>
  default_action: 'allow' | 'deny'
}

export interface ForwardDestinationPolicyConfig {
  allow_domains: string[]
  deny_domains: string[]
  allow_cidrs: string[]
  deny_cidrs: string[]
  deny_private: boolean
}

export interface ForwardResolverPolicyConfig {
  nameservers: string[]
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
  access_policy: ForwardAccessPolicyConfig | null
  destination_policy: ForwardDestinationPolicyConfig
  header_policy: { forwarded_for: 'preserve' | 'delete'; via: 'preserve' | 'delete' }
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
  max_connections: number | null
  management: ManagementConfig | null
  stats?: StatsConfig | null
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

export type ConfigFormat = 'kdl' | 'lua' | 'uci' | 'hocon'

export interface ConfigSnapshot {
  schemaVersion: 1
  diskRevision: string
  candidateRevision: string
  activeRevision: string | null
  config: CanonicalConfig
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
  diagnostics: ConfigDiagnostic[]
}

export interface ConfigValidationResponse {
  candidateRevision: string
  normalizedConfig: CanonicalConfig
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
  diagnostics: ConfigDiagnostic[]
  restartRequired: boolean
  topology: CandidateTopologySnapshot
}

export type ConfigSaveOutcome = 'saved_pending_activation' | 'saved_restart_required' | 'unchanged_active'
export type ConfigActivationState = 'pending' | 'restart_required' | 'active'

export interface ConfigSaveResponse {
  diskRevision: string
  candidateRevision: string
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
      arrayOf(source.allowed_dns_suffixes, isString)
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
    (value.policy.session_cache === null || (isRecord(value.policy.session_cache) &&
      typeof value.policy.session_cache.name === 'string' && safeInteger(value.policy.session_cache.size_bytes))) &&
    nullableSafeInteger(value.policy.session_timeout_seconds) &&
    typeof value.policy.session_tickets === 'boolean' && typeof value.policy.prefer_server_ciphers === 'boolean'
}

function isListener(value: unknown): value is ListenerConfig {
  return isRecord(value) && typeof value.name === 'string' && isListenerBind(value.bind) &&
    ['http', 'tcp', 'rtmp', 'forward_http1', 'forward_http2', 'forward_http3']
      .includes(String(value.protocol)) && nullableString(value.service) &&
    nullableString(value.tls_profile) && nullableSafeInteger(value.max_connections) &&
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

function isUpstreamPool(value: unknown): value is UpstreamPoolConfig {
  return isRecord(value) && typeof value.name === 'string' && arrayOf(value.servers, isUpstreamServer) &&
    (value.endpoints === undefined || arrayOf(value.endpoints, isEndpoint)) &&
    ['round_robin', 'least_connections', 'first'].includes(String(value.algorithm)) &&
    (value.health_check === null || isHealthCheck(value.health_check)) &&
    (value.tls === null || (isRecord(value.tls) && typeof value.tls.server_name === 'string' &&
      nullableString(value.tls.ca_certificate_path))) && isRecord(value.http_versions) &&
    ['1.1', '2'].includes(String(value.http_versions.min)) &&
    ['1.1', '2'].includes(String(value.http_versions.max)) &&
    nullableSafeInteger(value.queue_timeout_ms) && nullableSafeInteger(value.connect_timeout_ms) &&
    nullableSafeInteger(value.server_timeout_ms) &&
    ['never', 'safe', 'always'].includes(String(value.connection_reuse))
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
  return isRecord(value) && isHttpUpstreamHost(value.upstream_host) &&
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
    (value.access_log === null || isAccessLog(value.access_log)) && arrayOf(value.applications, (application) =>
    isRecord(application) && typeof application.name === 'string' && typeof application.live === 'boolean' &&
    typeof application.idle_streams === 'boolean' && arrayOf(application.push_targets, (target) =>
      isRecord(target) && typeof target.host === 'string' && safeInteger(target.port) &&
      typeof target.application === 'string') && isRecord(application.fanout) &&
    safeInteger(application.fanout.max_subscribers) &&
    safeInteger(application.fanout.max_queue_messages_per_subscriber) &&
    safeInteger(application.fanout.max_queue_bytes_per_subscriber) &&
    arrayOf(application.recorders, isRtmpRecorder))
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
    (value.auth === null || isForwardProxyAuth(value.auth)) &&
    (value.access_policy === null || isForwardAccessPolicy(value.access_policy)) &&
    isRecord(value.destination_policy) &&
    arrayOf(value.destination_policy.allow_domains, isString) &&
    arrayOf(value.destination_policy.deny_domains, isString) &&
    arrayOf(value.destination_policy.allow_cidrs, isString) &&
    arrayOf(value.destination_policy.deny_cidrs, isString) &&
    typeof value.destination_policy.deny_private === 'boolean' &&
    isRecord(value.header_policy) && ['preserve', 'delete'].includes(String(value.header_policy.forwarded_for)) &&
    ['preserve', 'delete'].includes(String(value.header_policy.via)) &&
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
    nullableSafeInteger(value.credential_ttl_ms) && typeof value.username_case_sensitive === 'boolean'))
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

function isRtmpRecorder(value: unknown): value is RtmpRecorderConfig {
  return isRecord(value) && typeof value.name === 'string' &&
    ['continuous', 'manual'].includes(String(value.start)) && typeof value.root_directory === 'string' &&
    typeof value.suffix_template === 'string' && typeof value.append_unix_seconds === 'boolean' &&
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
  { path: 'stats', kind: 'object' },
  { path: 'stats.binds', kind: 'string_list' },
  { path: 'stats.admin_token_file', kind: 'string' },
  { path: 'stats.pages', kind: 'collection' },
  { path: 'stats.pages[].bind', kind: 'string' },
  { path: 'stats.pages[].uri_prefix', kind: 'string' },
  { path: 'stats.pages[].refresh_ms', kind: 'integer' },
  { path: 'stats.pages[].admin', kind: 'enum' },
  { path: 'stats.pages[].max_connections', kind: 'integer' },
  { path: 'stats.pages[].downstream_timeouts', kind: 'object' },
  { path: 'stats.pages[].downstream_timeouts.client_timeout_ms', kind: 'integer' },
  { path: 'stats.pages[].downstream_timeouts.request_timeout_ms', kind: 'integer' },
  { path: 'stats.pages[].downstream_timeouts.keepalive_timeout_ms', kind: 'integer' },
  { path: 'certificates', kind: 'collection' },
  { path: 'certificates[].name', kind: 'string' },
  { path: 'certificates[].dns_names', kind: 'string_list' },
  { path: 'certificates[].source', kind: 'object' },
  { path: 'certificates[].source.type', kind: 'enum' },
  { path: 'certificates[].source.certificate_chain_path', kind: 'string' },
  { path: 'certificates[].source.private_key_path', kind: 'string' },
  { path: 'certificates[].source.live_directory_path', kind: 'string' },
  { path: 'certificates[].source.archive_directory_path', kind: 'string' },
  { path: 'certificates[].source.directory_url', kind: 'string' },
  { path: 'certificates[].source.state_root', kind: 'string' },
  { path: 'certificates[].source.contacts', kind: 'string_list' },
  { path: 'certificates[].source.terms_agreed', kind: 'boolean' },
  { path: 'certificates[].source.challenge', kind: 'enum' },
  { path: 'certificates[].source.allowed_dns_suffixes', kind: 'string_list' },
  { path: 'certificates[].source.validity_days', kind: 'integer' },
  { path: 'certificates[].source.key_type', kind: 'enum' },
  { path: 'tls_profiles', kind: 'collection' },
  { path: 'tls_profiles[].name', kind: 'string' },
  { path: 'tls_profiles[].certificates', kind: 'string_list' },
  { path: 'tls_profiles[].default_certificate', kind: 'reference' },
  { path: 'tls_profiles[].min_version', kind: 'enum' },
  { path: 'tls_profiles[].alpn', kind: 'enum' },
  { path: 'tls_profiles[].policy', kind: 'object' },
  { path: 'tls_profiles[].policy.cipher_list', kind: 'string' },
  { path: 'tls_profiles[].policy.dh_parameters_path', kind: 'string' },
  { path: 'tls_profiles[].policy.session_cache', kind: 'object' },
  { path: 'tls_profiles[].policy.session_cache.name', kind: 'string' },
  { path: 'tls_profiles[].policy.session_cache.size_bytes', kind: 'integer' },
  { path: 'tls_profiles[].policy.session_timeout_seconds', kind: 'integer' },
  { path: 'tls_profiles[].policy.session_tickets', kind: 'boolean' },
  { path: 'tls_profiles[].policy.prefer_server_ciphers', kind: 'boolean' },
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
  { path: 'http_services[].routes[].action.policy.upstream_host.fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.upstream_host.unix_fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.upstream_host.value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.request_headers[].operation', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.request_headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.type', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.except_source_cidrs', kind: 'string_list' },
  { path: 'http_services[].routes[].action.policy.response_headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.response_headers[].operation', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.response_headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_headers[].value', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_headers[].always', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites[].from', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_cookie_path_rewrites[].to', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.retry', kind: 'object' },
  { path: 'http_services[].routes[].action.policy.retry.max_retries', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.retry.target', kind: 'enum' },
  { path: 'http_services[].routes[].action.policy.retry.delay_ms', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.retry.final_redispatch', kind: 'boolean' },
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
  { path: 'http_services[].routes[].action.headers[].always', kind: 'boolean' },
  { path: 'http_services[].routes[].action.location', kind: 'object' },
  { path: 'http_services[].routes[].action.location.kind', kind: 'enum' },
  { path: 'http_services[].routes[].action.location.value', kind: 'string' },
  { path: 'http_services[].routes[].action.location.nginx_host_fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.root_directory', kind: 'string' },
  { path: 'http_services[].routes[].action.index_files', kind: 'string_list' },
  { path: 'http_services[].routes[].action.internal_index_redirects', kind: 'boolean' },
  { path: 'http_services[].routes[].action.directory_redirects', kind: 'boolean' },
  { path: 'http_services[].routes[].action.spa_fallback', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses[].internal_redirect', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses[].body', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses[].headers', kind: 'collection' },
  { path: 'http_services[].routes[].action.error_responses[].headers[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses[].headers[].value', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses[].headers[].always', kind: 'boolean' },
  { path: 'http_services[].automatic_response_headers', kind: 'boolean' },
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
  { path: 'forward_proxy_services[].auth.htpasswd_file_path', kind: 'string' },
  { path: 'forward_proxy_services[].auth.realm', kind: 'string' },
  { path: 'forward_proxy_services[].auth.credential_ttl_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].auth.username_case_sensitive', kind: 'boolean' },
  { path: 'forward_proxy_services[].access_policy', kind: 'object' },
  { path: 'forward_proxy_services[].access_policy.default_action', kind: 'enum' },
  { path: 'forward_proxy_services[].access_policy.rules', kind: 'collection' },
  { path: 'forward_proxy_services[].access_policy.rules[].action', kind: 'enum' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions', kind: 'collection' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].negated', kind: 'boolean' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].type', kind: 'enum' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].methods', kind: 'string_list' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].cidrs', kind: 'string_list' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].ranges', kind: 'collection' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].ranges[].start', kind: 'integer' },
  { path: 'forward_proxy_services[].access_policy.rules[].conditions[].ranges[].end', kind: 'integer' },
  { path: 'forward_proxy_services[].destination_policy', kind: 'object' },
  { path: 'forward_proxy_services[].destination_policy.allow_domains', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_domains', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.allow_cidrs', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_cidrs', kind: 'string_list' },
  { path: 'forward_proxy_services[].destination_policy.deny_private', kind: 'boolean' },
  { path: 'forward_proxy_services[].header_policy', kind: 'object' },
  { path: 'forward_proxy_services[].header_policy.forwarded_for', kind: 'enum' },
  { path: 'forward_proxy_services[].header_policy.via', kind: 'enum' },
  { path: 'forward_proxy_services[].connect_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].idle_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].lifetime_timeout_ms', kind: 'integer' },
  { path: 'forward_proxy_services[].max_request_body_bytes', kind: 'integer' },
  { path: 'forward_proxy_services[].max_header_bytes', kind: 'integer' },
  { path: 'forward_proxy_services[].max_connections', kind: 'integer' },
  { path: 'forward_proxy_services[].resolver', kind: 'object' },
  { path: 'forward_proxy_services[].resolver.nameservers', kind: 'string_list' },
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
  { path: 'max_connections', kind: 'integer' },
  { path: 'listeners[].bind.mode', kind: 'integer' },
  { path: 'listeners[].downstream_timeouts', kind: 'object' },
  { path: 'listeners[].downstream_timeouts.client_timeout_ms', kind: 'integer' },
  { path: 'listeners[].downstream_timeouts.request_timeout_ms', kind: 'integer' },
  { path: 'listeners[].downstream_timeouts.keepalive_timeout_ms', kind: 'integer' },
  { path: 'upstream_pools[].servers', kind: 'collection' },
  { path: 'upstream_pools[].servers[].name', kind: 'string' },
  { path: 'upstream_pools[].servers[].endpoint', kind: 'object' },
  { path: 'upstream_pools[].servers[].endpoint.type', kind: 'enum' },
  { path: 'upstream_pools[].servers[].endpoint.address', kind: 'string' },
  { path: 'upstream_pools[].servers[].endpoint.host', kind: 'string' },
  { path: 'upstream_pools[].servers[].endpoint.port', kind: 'integer' },
  { path: 'upstream_pools[].servers[].endpoint.path', kind: 'string' },
  { path: 'upstream_pools[].servers[].max_connections', kind: 'integer' },
  { path: 'upstream_pools[].servers[].dns_resolution', kind: 'enum' },
  { path: 'upstream_pools[].queue_timeout_ms', kind: 'integer' },
  { path: 'upstream_pools[].connect_timeout_ms', kind: 'integer' },
  { path: 'upstream_pools[].server_timeout_ms', kind: 'integer' },
  { path: 'upstream_pools[].connection_reuse', kind: 'enum' },
  { path: 'upstream_pools[].health_check.startup', kind: 'enum' },
  { path: 'upstream_pools[].health_check.fast_interval_ms', kind: 'integer' },
  { path: 'upstream_pools[].health_check.down_interval_ms', kind: 'integer' },
  { path: 'upstream_pools[].health_check.expected_status', kind: 'integer' },
  { path: 'upstream_pools[].health_check.http_version', kind: 'enum' },
  { path: 'http_services[].routes[].access_policy.htpasswd_file_path', kind: 'string' },
  { path: 'http_services[].routes[].policy', kind: 'object' },
  { path: 'http_services[].routes[].policy.max_request_body_bytes', kind: 'integer' },
  { path: 'http_services[].routes[].policy.connect_timeout_ms', kind: 'integer' },
  { path: 'http_services[].routes[].policy.read_timeout_ms', kind: 'integer' },
  { path: 'http_services[].routes[].policy.write_timeout_ms', kind: 'integer' },
  { path: 'http_services[].routes[].policy.request_buffering', kind: 'boolean' },
  { path: 'http_services[].routes[].policy.response_buffering', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.request_headers[].value.max_bytes', kind: 'integer' },
  { path: 'http_services[].routes[].action.policy.response_cookie_attributes', kind: 'collection' },
  { path: 'http_services[].routes[].action.policy.response_cookie_attributes[].name', kind: 'string' },
  { path: 'http_services[].routes[].action.policy.response_cookie_attributes[].secure', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.response_cookie_attributes[].http_only', kind: 'boolean' },
  { path: 'http_services[].routes[].action.policy.response_cookie_attributes[].same_site', kind: 'enum' },
  { path: 'http_services[].routes[].action.path_mapping', kind: 'enum' },
  { path: 'http_services[].routes[].action.try_files', kind: 'collection' },
  { path: 'http_services[].routes[].action.try_files[].type', kind: 'enum' },
  { path: 'http_services[].routes[].action.try_files[].path', kind: 'string' },
  { path: 'http_services[].routes[].action.try_files[].status', kind: 'integer' },
  { path: 'http_services[].routes[].action.autoindex', kind: 'boolean' },
  { path: 'http_services[].routes[].action.autoindex_exact_size', kind: 'boolean' },
  { path: 'http_services[].routes[].action.autoindex_local_time', kind: 'boolean' },
  { path: 'http_services[].routes[].action.etag', kind: 'boolean' },
  { path: 'http_services[].routes[].action.mime', kind: 'object' },
  { path: 'http_services[].routes[].action.mime.default_type', kind: 'string' },
  { path: 'http_services[].routes[].action.mime.types', kind: 'collection' },
  { path: 'http_services[].routes[].action.mime.types[].extension', kind: 'string' },
  { path: 'http_services[].routes[].action.mime.types[].content_type', kind: 'string' },
  { path: 'http_services[].routes[].action.error_responses', kind: 'collection' },
  { path: 'http_services[].routes[].action.error_responses[].statuses', kind: 'collection' },
  { path: 'http_services[].routes[].action.error_responses[].file', kind: 'string' },
  { path: 'http_services[].gzip', kind: 'object' },
  { path: 'http_services[].gzip.level', kind: 'integer' },
  { path: 'http_services[].gzip.content_types', kind: 'string_list' },
  { path: 'http_services[].gzip.min_length_bytes', kind: 'integer' },
  { path: 'http_services[].gzip.min_http_version', kind: 'enum' },
  { path: 'http_services[].gzip.disable_on_via', kind: 'boolean' },
  { path: 'http_services[].gzip.vary', kind: 'boolean' },
  { path: 'http_services[].access_log', kind: 'object' },
  { path: 'http_services[].access_log.type', kind: 'enum' },
  { path: 'http_services[].access_log.path', kind: 'string' },
  { path: 'rtmp_services[].outbound_chunk_size', kind: 'integer' },
  { path: 'rtmp_services[].access_log', kind: 'object' },
  { path: 'rtmp_services[].access_log.type', kind: 'enum' },
  { path: 'rtmp_services[].access_log.path', kind: 'string' },
  { path: 'rtmp_services[].applications[].push_targets', kind: 'collection' },
  { path: 'rtmp_services[].applications[].push_targets[].host', kind: 'string' },
  { path: 'rtmp_services[].applications[].push_targets[].port', kind: 'integer' },
  { path: 'rtmp_services[].applications[].push_targets[].application', kind: 'string' },
  { path: 'rtmp_services[].applications[].fanout', kind: 'object' },
  { path: 'rtmp_services[].applications[].fanout.max_subscribers', kind: 'integer' },
  { path: 'rtmp_services[].applications[].fanout.max_queue_messages_per_subscriber', kind: 'integer' },
  { path: 'rtmp_services[].applications[].fanout.max_queue_bytes_per_subscriber', kind: 'integer' },
  { path: 'rtmp_services[].applications[].recorders[].timezone', kind: 'enum' },
  { path: 'rtmp_services[].applications[].recorders[].time_basis', kind: 'enum' },
  { path: 'rtmp_services[].applications[].recorders[].segment_naming', kind: 'enum' },
] as const satisfies readonly CanonicalFieldDefinition[]
