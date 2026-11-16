import type { RtmpServiceConfig } from './rtmp'

export * from './rtmp'

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
export const UPSTREAM_WEIGHT_MIN = 1
export const UPSTREAM_WEIGHT_MAX = 100
export interface WeightedRoundRobinAlgorithm {
  type: 'weighted_round_robin'
  weights: number[]
}
export type UpstreamAlgorithm =
  | 'round_robin'
  | 'least_connections'
  | 'power_of_two_choices'
  | 'first'
  | WeightedRoundRobinAlgorithm
export type AccessLogConfig = { type: 'disabled' } | { type: 'file'; path: string }

export interface ManagementConfig {
  bind: string
  ui_dir: string | null
  token_file?: string
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

export interface AcmeDns01Config {
  provider: string
  credential_file: string
  timeout_seconds: number
}

export interface AcmeManagedCertificateSource {
  type: 'acme_managed'
  directory_url: string
  state_root: string
  contacts: string[]
  terms_agreed: boolean
  challenge: AcmeChallengeType
  key_type: AcmeKeyType
  allowed_dns_suffixes: string[]
  retained_revisions: number
  retention_days: number
  dns01: AcmeDns01Config | null
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
  client_auth: TlsClientAuthPolicyConfig
  session_cache: TlsSessionCacheConfig | null
  session_timeout_seconds: number | null
  session_tickets: boolean
  prefer_server_ciphers: boolean
  backend?: TlsBackend | null
  key_exchange_groups?: string[] | null
  ktls: KtlsMode
}

export type TlsBackend = 'openssl' | 'rustls'

export type KtlsMode = 'off' | 'auto'

export type TlsClientAuthMode = 'disabled' | 'optional' | 'required'

export interface TlsClientAuthPolicyConfig {
  mode: TlsClientAuthMode
  ca_certificate_path: string | null
  allowed_dns_names: string[]
}

export interface TlsSessionCacheConfig {
  name: string
  size_bytes: number
}

export interface SocketListenerBind {
  type: 'socket'
  address: string
  ipv6_only?: boolean
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

export type ProxyProtocolVersion = 'v1' | 'v2' | 'auto'

export interface ProxyProtocolPolicyConfig {
  version: ProxyProtocolVersion
  timeout_ms: number
}

export interface ListenerConfig {
  name: string
  bind: ListenerBind
  protocol: ListenerProtocol
  service: string | null
  tls_profile: string | null
  proxy_protocol?: ProxyProtocolPolicyConfig | null
  max_connections: number | null
  downstream_timeouts: {
    client_timeout_ms: number | null
    request_timeout_ms: number | null
    keepalive_timeout_ms: number | null
  }
  h2_initial_stream_window?: number
  h2_initial_connection_window?: number
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

export interface PassiveHealthConfig {
  observe: 'layer4' | 'layer7'
  on_error: 'count' | 'immediately' | 'mark_down'
  error_limit: number
  mark_down: boolean
  mark_up: boolean
  initial_backoff_ms: number
  max_backoff_ms: number
  recovery_threshold: number
}

export interface PoolResilienceConfig {
  fail_open: boolean
  min_members: number | null
  ejection_jitter_percent: number
  queue_while_unavailable_ms: number | null
  unavailable_retry_after_seconds: number | null
  unavailable_body: string | null
}

export type UpstreamTlsVerify = 'full' | 'none'

export interface UpstreamTlsConfig {
  server_name: string | null
  verify?: UpstreamTlsVerify
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
  passive_health: PassiveHealthConfig | null
  tls: UpstreamTlsConfig | null
  http_versions: HttpVersionPolicyConfig
  queue_timeout_ms: number | null
  connect_timeout_ms: number | null
  server_timeout_ms: number | null
  connection_reuse: 'never' | 'safe' | 'always'
  resilience: PoolResilienceConfig
  queue?: PoolQueueConfig | null
  registration?: PoolRegistrationConfig | null
  canary?: PoolCanaryConfig | null
}

export interface PoolCanaryConfig {
  path: string
  host: string | null
  expected_status: number
  interval_ms: number
  timeout_ms: number
  max_latency_ms: number | null
}

export interface PoolQueueConfig {
  classes: { name: string; weight: number }[]
  discipline: 'weighted' | 'strict'
  class_header: string | null
  default_class: string | null
  tenant_header: string | null
  max_tenants: number
  aging_ms: number | null
}

export interface PoolRegistrationConfig {
  max_servers: number
  min_ttl_ms: number
  default_ttl_ms: number
  max_ttl_ms: number
  allowed_cidrs: string[]
  require_peer_address: boolean
}

export type RegistryEndpointConfig =
  | { type: 'unix'; path: string; mode: number; role: 'read' | 'register'; pools: string[] }
  | { type: 'tcp'; bind: string; allow_cidrs: string[] }

export interface RegistryConfig {
  state_dir: string
  tokens_file: string | null
  endpoints: RegistryEndpointConfig[]
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
  'empty_response',
  'response_timeout',
  'junk_response',
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

export type CookiePathMatch = 'exact' | 'nginx_prefix'

export interface HttpCookiePathRewriteConfig {
  from: string
  to: string
  mode?: CookiePathMatch
}

export interface HttpProxyPathRewriteConfig {
  from: string
  to: string
}

export interface HttpCookieAttributePolicyConfig {
  name: string
  secure: boolean | null
  http_only: boolean | null
  same_site: 'strict' | 'lax' | 'none' | null
}

export interface HttpRetryBudgetConfig {
  per_second: number | null
  ratio_percent: number | null
}

export interface HttpRetryPolicyConfig {
  max_retries: number
  target: 'same_server' | 'next_server'
  delay_ms: number
  jitter_percent: number
  final_redispatch: boolean
  idempotency_header: string | null
  budget: HttpRetryBudgetConfig | null
  triggers: HttpRetryTrigger[]
  response_statuses?: number[]
  method_safety: 'get_head' | 'all'
  body_safety: 'empty' | 'buffered'
}

export interface HttpProxyTimeoutsConfig {
  connect_ms: number | null
  header_ms: number | null
  idle_ms: number | null
  total_ms: number | null
}

export interface HttpRescuePolicyConfig {
  statuses: number[]
  target: 'next_server' | 'pool'
  pool: string | null
}

export interface HttpPoolSelectConfig {
  header: string
  rules: { at_most: number; pool: string }[]
  on_invalid: 'default' | 'reject'
  forward: boolean
  spill: 'none' | 'up'
}

export type HttpAffinityConfig =
  | { type: 'cookie'; name: string; ttl_s: number; same_site: 'strict' | 'lax' | 'none'; secure: boolean; http_only: boolean }
  | { type: 'hash'; key: string }
  | { type: 'header'; name: string }

export interface HttpProxyPolicyConfig {
  upstream_host: HttpUpstreamHostConfig
  upstream_path_rewrite?: HttpProxyPathRewriteConfig | null
  request_headers: HttpRequestHeaderMutationConfig[]
  response_headers: HttpResponseHeaderMutationConfig[]
  response_cookie_path_rewrites: HttpCookiePathRewriteConfig[]
  response_cookie_attributes: HttpCookieAttributePolicyConfig[]
  retry: HttpRetryPolicyConfig
  timeouts: HttpProxyTimeoutsConfig
  rescue: HttpRescuePolicyConfig | null
  cache: HttpCachePolicyConfig | null
  select?: HttpPoolSelectConfig | null
  affinity?: HttpAffinityConfig | null
  queue_headers?: boolean
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
  compression?: CacheCompressionConfig | null
}

export type CacheCompressionCoding = 'zstd' | 'br' | 'gzip'

export interface CacheCompressionConfig {
  codings: CacheCompressionCoding[]
  precompress: CacheCompressionCoding[]
  content_types: string[]
  min_length_bytes: number
  gzip_level: number
  brotli_level: number
  zstd_level: number
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

export interface HttpRoutePolicyConfig {
  max_request_body_bytes: number | null
  connect_timeout_ms: number
  read_timeout_ms: number
  write_timeout_ms: number
  request_buffering: boolean
  response_buffering: boolean
}

export interface HttpRouteConfig {
  host: HttpHostSelectorConfig | null
  path: HttpPathSelectorConfig
  methods: string[]
  access_policy: HttpAccessPolicyConfig | null
  policy: HttpRoutePolicyConfig
  action: HttpRouteActionConfig
}

export interface HttpServiceConfig {
  name: string
  drain_deadline_ms?: number | null
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


export type ForwardHttpVersion = 'h1' | 'h2' | 'h3'

export interface ForwardConnectPolicyConfig {
  enabled: boolean
  allowed_ports: number[]
}

export interface ForwardPeerConfig {
  host: string
  port: number
}

export interface ForwardPeerPolicyConfig {
  peers: ForwardPeerConfig[]
  direct_fallback: 'allowed' | 'denied' | 'required'
  max_retries: number
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
  | { type: 'mutual_tls'; client_ca_file_path: string }

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
  allow_times: ForwardTimeRangeConfig[]
  deny_times: ForwardTimeRangeConfig[]
}

export interface ForwardTimeRangeConfig {
  days: ForwardWeekday[]
  start: string
  end: string
}

export type ForwardWeekday = 'monday' | 'tuesday' | 'wednesday' | 'thursday' | 'friday' | 'saturday' | 'sunday'

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
  drain_deadline_ms?: number | null
  enabled_versions: ForwardHttpVersion[]
  allow_absolute_form: boolean
  tls_required: boolean
  connect: ForwardConnectPolicyConfig
  connect_udp: ForwardConnectPolicyConfig
  peer_policy: ForwardPeerPolicyConfig
  auth: ForwardProxyAuthConfig | null
  access_policy: ForwardAccessPolicyConfig | null
  destination_policy: ForwardDestinationPolicyConfig
  header_policy: { forwarded_for: 'preserve' | 'delete'; via: 'preserve' | 'delete' }
  cache?: HttpCachePolicyConfig | null
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
  drain_deadline_ms?: number | null
  upstream_pool: string
  connect_timeout_ms: number
  idle_timeout_ms: number
  lifetime_timeout_ms: number | null
  proxy_protocol?: ProxyProtocolPolicyConfig | null
  udp: UdpPolicyConfig | null
}

export interface UdpPolicyConfig {
  max_datagram_bytes: number
  max_sessions: number
  max_session_bytes: number
  max_queue_datagrams: number
  max_queue_bytes: number
}

export type HttpDataPlane = 'epoll' | 'io_uring' | 'tokio'

export type ConnectionScheduling = 'work_stealing' | 'aged_per_worker' | 'auto'
export type CongestionControl = 'cubic' | 'bbr'

export type WorkerScaling = 'static' | 'dynamic'

export interface RetirementConfig {
  drain_deadline_ms: number | null
  max_retired_generations: number
}

export interface AuditConfig {
  dir?: string
  max_records?: number
  max_record_bytes?: number
  max_file_bytes?: number
  max_total_bytes?: number
  max_rotated_files?: number
}

export interface CanonicalConfig {
  version: number
  max_connections: number | null
  listener_workers?: number
  http_fast_lane?: boolean
  http_data_plane?: HttpDataPlane
  connection_scheduling?: ConnectionScheduling
  connection_age_ms?: number
  worker_scaling?: WorkerScaling
  min_active_workers?: number
  target_busy_percent?: number
  scale_down_delay_ms?: number
  tls_handshake_threads?: number
  http3_congestion_control?: CongestionControl
  http3_segmentation_offload?: boolean
  retirement?: RetirementConfig
  management: ManagementConfig | null
  log_filter?: string
  audit?: AuditConfig
  registry?: RegistryConfig | null
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
