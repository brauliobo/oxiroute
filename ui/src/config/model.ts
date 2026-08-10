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
  | 'first'
  | WeightedRoundRobinAlgorithm
export type RtmpRecorderStart = 'continuous' | 'manual'
export type RtmpAclAction = 'allow' | 'deny'
export type RtmpTokenSource = 'stream_query'
export type RtmpNotifyMethod = 'get' | 'post'
export type RtmpRtmpsPolicy = 'disabled' | 'allowed' | 'required'
export type RtmpTransport = 'rtmp' | 'rtmps'
export type RtmpExecMode = 'command' | 'transcode'
export type RtmpExecTrigger = 'publisher' | 'publish_done'
export type RtmpExecFilesystemPolicy = 'working_directory' | 'host'
export type RtmpExecNetworkPolicy = 'disabled' | 'inherited'
export type RtmpVodSource =
  | { type: 'local'; name: string; root_directory: string }
  | { type: 'http'; name: string; origin: string }
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
}

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
  passive_health: PassiveHealthConfig | null
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

export interface HttpCookiePathRewriteConfig {
  from: string
  to: string
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

export interface HttpRetryPolicyConfig {
  max_retries: number
  target: 'same_server' | 'next_server'
  delay_ms: number
  final_redispatch: boolean
  triggers: HttpRetryTrigger[]
  response_statuses?: number[]
  method_safety: 'get_head'
  body_safety: 'empty'
}

export interface HttpProxyPolicyConfig {
  upstream_host: HttpUpstreamHostConfig
  upstream_path_rewrite?: HttpProxyPathRewriteConfig | null
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

export type RtmpHlsFragmentNaming = 'sequential' | 'timestamp' | 'system'

export interface RtmpHlsVariantConfig {
  name: string
  bandwidth: number
  codecs: string | null
  width: number | null
  height: number | null
}

export interface RtmpHlsKeyConfig {
  rotation_segments: number
  url_prefix: string
}

export interface RtmpHlsPolicyConfig {
  root_directory: string
  segment_duration_ms: number
  max_segment_duration_ms: number
  playlist_length_ms: number
  fragment_naming: RtmpHlsFragmentNaming
  nested: boolean
  cleanup: boolean
  variants: RtmpHlsVariantConfig[]
  keys: RtmpHlsKeyConfig | null
  max_segment_bytes: number
  max_queue_messages: number
  max_storage_bytes: number
  max_storage_files: number
  max_active_streams: number
}

export interface RtmpDashPolicyConfig {
  root_directory: string
  segment_duration_ms: number
  max_segment_duration_ms: number
  playlist_length_ms: number
  segment_naming: 'sequential' | 'timestamp' | 'system'
  nested: boolean
  cleanup: boolean
  max_segment_bytes: number
  max_queue_messages: number
  max_storage_bytes: number
  max_storage_files: number
  max_active_streams: number
}

export interface RtmpApplicationConfig {
  name: string
  live: boolean
  idle_streams: boolean
  publish: RtmpAccessPolicyConfig
  play: RtmpAccessPolicyConfig
  limits: RtmpSessionCeilingsConfig
  push_targets: RtmpPushTargetConfig[]
  pull_targets: RtmpPullTargetConfig[]
  relay: RtmpRelayPolicyConfig
  callbacks: RtmpCallbackConfig
  fanout: {
    max_subscribers: number
    max_queue_messages_per_subscriber: number
    max_queue_bytes_per_subscriber: number
  }
  vod: RtmpVodPolicyConfig | null
  hls?: RtmpHlsPolicyConfig | null
  dash?: RtmpDashPolicyConfig | null
  recorders: RtmpRecorderConfig[]
}

export interface RtmpAccessPolicyConfig {
  rules: RtmpAccessRuleConfig[]
  token: RtmpTokenPolicyConfig | null
}

export interface RtmpAccessRuleConfig {
  action: RtmpAclAction
  network: string
}

export interface RtmpTokenPolicyConfig {
  source: RtmpTokenSource
  parameter: string
  secret: string
}

export interface RtmpSessionCeilingsConfig {
  max_connections: number
  max_publishers: number
  max_viewers: number
}

export interface RtmpRecorderConfig {
  name: string
  start: RtmpRecorderStart
  root_directory: string
  record_mask: { audio: boolean; video: boolean; keyframes: boolean }
  suffix_template: string
  append_unix_seconds: boolean
  append: boolean
  lock: boolean
  max_size: number | null
  max_frames: number | null
  notify: boolean
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

export interface RtmpCallbackConfig {
  on_connect: string | null
  on_disconnect: string | null
  on_publish: string | null
  on_publish_done: string | null
  on_play: string | null
  on_play_done: string | null
  on_done: string | null
  on_update: string | null
  notify_method: RtmpNotifyMethod
  timeout_ms: number
  notify_update_timeout_ms: number
  notify_update_strict: boolean
  notify_relay_redirect: boolean
}

export interface RtmpOutboundPolicyConfig {
  allow_domains: string[]
  deny_domains: string[]
  allow_cidrs: string[]
  deny_cidrs: string[]
  deny_private: boolean
  rtmps: RtmpRtmpsPolicy
  max_chain_depth: number
}

export interface RtmpRelayPolicyConfig {
  max_queue_messages: number
  max_queue_bytes: number
  buffer_ms: number
  push_reconnect_ms: number
  pull_reconnect_ms: number
  dns_refresh_ms: number
  connect_timeout_ms: number
  handshake_timeout_ms: number
}

export interface RtmpCredentialReferenceConfig {
  username: string
  secret_file: string
}

export interface RtmpPushTargetConfig {
  host: string
  port: number
  application: string
  scheme: RtmpTransport
  stream_name: string | null
  tc_url: string | null
  flash_version: string | null
  credentials: RtmpCredentialReferenceConfig | null
}

export interface RtmpPullTargetConfig {
  host: string
  port: number
  application: string
  stream_name: string
  scheme: RtmpTransport
  tc_url: string | null
  flash_version: string | null
  credentials: RtmpCredentialReferenceConfig | null
}

export interface RtmpVodPolicyConfig {
  sources: RtmpVodSource[]
  max_sessions: number
  max_file_bytes: number
  max_duration_ms: number
}

export interface RtmpExecEnvironmentConfig {
  name: string
  value: string
}

export interface RtmpExecProfileConfig {
  name: string
  application: string
  mode: RtmpExecMode
  trigger: RtmpExecTrigger
  executable: string
  arguments: string[]
  environment: RtmpExecEnvironmentConfig[]
  working_directory: string
  filesystem: RtmpExecFilesystemPolicy
  network: RtmpExecNetworkPolicy
  timeout_ms: number
  shutdown_timeout_ms: number
  max_processes: number
  max_queue_messages: number
  max_queue_bytes: number
  max_stdout_bytes: number
  max_stderr_bytes: number
  respawn: boolean
  respawn_delay_ms: number
  max_respawns: number
}

export interface RtmpAutoPushConfig {
  enabled: boolean
  socket_dir: string
  secret_file: string | null
  reconnect_ms: number
  connect_timeout_ms: number
  handshake_timeout_ms: number
  max_peers: number
  max_queue_messages: number
  max_queue_bytes: number
  max_streams: number
}

export interface RtmpServiceConfig {
  name: string
  outbound_chunk_size: number
  access_log: AccessLogConfig | null
  outbound_policy: RtmpOutboundPolicyConfig
  callbacks: RtmpCallbackConfig
  auto_push: RtmpAutoPushConfig
  exec_profiles?: RtmpExecProfileConfig[]
  applications: RtmpApplicationConfig[]
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
