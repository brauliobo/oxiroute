import type {
  CacheStaleTrigger,
  CacheStoreConfig,
  ForwardHttpVersion,
  ForwardProxyServiceConfig,
  HttpCachePolicyConfig,
  RtmpAutoPushConfig,
} from '../config'

export const CACHE_STALE_TRIGGERS = [
  'connect_failure',
  'connect_timeout',
  'origin_500',
  'origin_502',
  'origin_503',
  'origin_504',
] as const satisfies readonly CacheStaleTrigger[]

export const FORWARD_HTTP_VERSIONS = ['h1', 'h2', 'h3'] as const satisfies readonly ForwardHttpVersion[]
export const FORWARD_WEEKDAYS = [
  'monday',
  'tuesday',
  'wednesday',
  'thursday',
  'friday',
  'saturday',
  'sunday',
] as const

const commonCacheLimits = () => ({
  max_object_bytes: 16_777_216,
  max_header_bytes: 65_536,
  max_key_bytes: 4_096,
  max_tag_bytes: 256,
  max_tags_per_object: 64,
  max_in_flight_fills: 1_024,
  max_followers_per_fill: 128,
})

export function defaultCacheStore(type: CacheStoreConfig['type']): CacheStoreConfig {
  return type === 'memory'
    ? {
        type,
        name: '',
        max_bytes: 268_435_456,
        max_entries: 100_000,
        ...commonCacheLimits(),
      }
    : {
        type,
        name: '',
        root_directory: '',
        max_bytes: 107_374_182_400,
        max_files: 1_000_000,
        ...commonCacheLimits(),
      }
}

export function defaultHttpCachePolicy(store = ''): HttpCachePolicyConfig {
  return {
    store,
    methods: ['GET', 'HEAD'],
    key_components: [{ type: 'scheme' }, { type: 'normalized_host' }, { type: 'path_and_query' }],
    use_origin_cache_control: true,
    default_ttl_ms: 60_000,
    status_ttls: [],
    grace_ms: 30_000,
    keep_ms: 300_000,
    revalidate: true,
    collapsed_forwarding: true,
    stale_on: [],
    bypass_request: [],
    no_store_request: [],
    no_store_response: [],
    set_cookie_policy: 'bypass',
    authorization_policy: 'bypass',
    vary_policy: 'respect',
    surrogate_tags: null,
    purge_authorization: null,
  }
}

export function defaultForwardProxyService(): ForwardProxyServiceConfig {
  return {
    name: '',
    enabled_versions: ['h1'],
    allow_absolute_form: true,
    tls_required: false,
    connect: { enabled: false, allowed_ports: [443] },
    auth: null,
    access_policy: null,
    destination_policy: {
      allow_domains: [],
      deny_domains: [],
      allow_cidrs: [],
      deny_cidrs: [],
      deny_private: true,
      allow_times: [],
      deny_times: [],
    },
    header_policy: { forwarded_for: 'preserve', via: 'preserve' },
    connect_timeout_ms: 10_000,
    idle_timeout_ms: 300_000,
    lifetime_timeout_ms: 3_600_000,
    max_request_body_bytes: 10_485_760,
    max_header_bytes: 65_536,
    max_connections: 10_000,
    resolver: {
      nameservers: [],
      max_cache_entries: 4_096,
      max_concurrent_queries: 256,
      max_addresses_per_name: 16,
      min_ttl_ms: 1_000,
      max_ttl_ms: 300_000,
      negative_ttl_ms: 30_000,
      revalidate_on_connect: true,
    },
    audit_mode: 'metadata',
  }
}

export function defaultRtmpCallback() {
  return {
    on_connect: null,
    on_disconnect: null,
    on_publish: null,
    on_publish_done: null,
    on_play: null,
    on_play_done: null,
    on_done: null,
    on_update: null,
    notify_method: 'post' as const,
    timeout_ms: 10_000,
    notify_update_timeout_ms: 30_000,
    notify_update_strict: false,
    notify_relay_redirect: false,
  }
}

export function defaultRtmpAutoPush(): RtmpAutoPushConfig {
  return {
    enabled: false,
    socket_dir: '/tmp/oxiroute-rtmp',
    secret_file: null,
    reconnect_ms: 100,
    connect_timeout_ms: 500,
    handshake_timeout_ms: 2_000,
    max_peers: 8,
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    max_streams: 1_024,
  }
}

export function defaultRtmpRelay() {
  return {
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    buffer_ms: 5_000,
    push_reconnect_ms: 3_000,
    pull_reconnect_ms: 3_000,
    connect_timeout_ms: 500,
    handshake_timeout_ms: 2_000,
  }
}

export function defaultRtmpOutboundPolicy() {
  return {
    allow_domains: [],
    deny_domains: [],
    allow_cidrs: [],
    deny_cidrs: [],
    deny_private: true,
    rtmps: 'disabled' as const,
    max_chain_depth: 4,
  }
}
