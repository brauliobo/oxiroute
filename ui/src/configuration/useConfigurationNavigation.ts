import { computed, type Ref } from 'vue'

import type {
  CacheStoreConfig,
  CanonicalConfig,
  ListenerConfig,
  UpstreamPoolConfig,
} from '../config'
import {
  defaultCacheStore,
  defaultForwardProxyService,
  defaultRtmpCallback,
  defaultRtmpAutoPush,
  defaultRtmpOutboundPolicy,
  defaultRtmpRelay,
} from './canonicalDefaults'
import { defaultHttpRoute } from './httpDefaults'

export type CollectionName =
  | 'certificates'
  | 'tls_profiles'
  | 'listeners'
  | 'cache_stores'
  | 'upstream_pools'
  | 'http_services'
  | 'forward_proxy_services'
  | 'rtmp_services'
  | 'l4_services'

export interface NavigationItem {
  key: string
  label: string
  detail: string
}

export interface NavigationGroup {
  collection: CollectionName
  label: string
  singular: string
  items: NavigationItem[]
}

export function useConfigurationNavigation(
  draft: Ref<CanonicalConfig | null>,
  selectedKey: Ref<string>,
  markChanged: () => void,
) {
  const selection = computed(() => {
    const [collection, rawIndex] = selectedKey.value.split(':')
    const index = Number(rawIndex)
    return Number.isInteger(index) ? { collection: collection as CollectionName, index } : null
  })

  const selectedCertificate = computed(() =>
    draft.value && selection.value?.collection === 'certificates'
      ? draft.value.certificates[selection.value.index] ?? null
      : null,
  )
  const selectedTlsProfile = computed(() =>
    draft.value && selection.value?.collection === 'tls_profiles'
      ? draft.value.tls_profiles[selection.value.index] ?? null
      : null,
  )
  const selectedListener = computed(() =>
    draft.value && selection.value?.collection === 'listeners'
      ? draft.value.listeners[selection.value.index] ?? null
      : null,
  )
  const selectedPool = computed(() =>
    draft.value && selection.value?.collection === 'upstream_pools'
      ? draft.value.upstream_pools[selection.value.index] ?? null
      : null,
  )
  const selectedCacheStore = computed(() =>
    draft.value && selection.value?.collection === 'cache_stores'
      ? draft.value.cache_stores[selection.value.index] ?? null
      : null,
  )
  const selectedHttpService = computed(() =>
    draft.value && selection.value?.collection === 'http_services'
      ? draft.value.http_services[selection.value.index] ?? null
      : null,
  )
  const selectedRtmpService = computed(() =>
    draft.value && selection.value?.collection === 'rtmp_services'
      ? draft.value.rtmp_services[selection.value.index] ?? null
      : null,
  )
  const selectedForwardProxyService = computed(() =>
    draft.value && selection.value?.collection === 'forward_proxy_services'
      ? draft.value.forward_proxy_services[selection.value.index] ?? null
      : null,
  )
  const selectedL4Service = computed(() =>
    draft.value && selection.value?.collection === 'l4_services'
      ? draft.value.l4_services[selection.value.index] ?? null
      : null,
  )

  const certificateNames = computed(() => draft.value?.certificates.map(({ name }) => name) ?? [])
  const tlsProfileNames = computed(() => draft.value?.tls_profiles.map(({ name }) => name) ?? [])
  const poolNames = computed(() => draft.value?.upstream_pools.map(({ name }) => name) ?? [])
  const cacheStoreNames = computed(() => draft.value?.cache_stores.map(({ name }) => name) ?? [])
  const forwardProxyServices = computed(() => draft.value?.forward_proxy_services ?? [])
  const l4PoolNames = computed(
    () => draft.value?.upstream_pools.filter(({ tls }) => tls === null).map(({ name }) => name) ?? [],
  )
  const httpServiceNames = computed(() => draft.value?.http_services.map(({ name }) => name) ?? [])
  const rtmpServiceNames = computed(() => draft.value?.rtmp_services.map(({ name }) => name) ?? [])
  const l4ServiceNames = computed(() => draft.value?.l4_services.map(({ name }) => name) ?? [])

  const navigationGroups = computed<NavigationGroup[]>(() => {
    if (!draft.value) return []
    return [
      navigationGroup('certificates', 'Certificates', 'certificate', draft.value.certificates),
      navigationGroup('tls_profiles', 'SNI profiles', 'SNI profile', draft.value.tls_profiles),
      navigationGroup('listeners', 'Listeners', 'listener', draft.value.listeners),
      navigationGroup('cache_stores', 'Cache stores', 'cache store', draft.value.cache_stores),
      navigationGroup('upstream_pools', 'Upstream pools', 'upstream pool', draft.value.upstream_pools),
      navigationGroup('http_services', 'HTTP services', 'HTTP service', draft.value.http_services),
      navigationGroup('forward_proxy_services', 'Forward proxies', 'forward proxy', draft.value.forward_proxy_services),
      navigationGroup('rtmp_services', 'RTMP services', 'RTMP service', draft.value.rtmp_services),
      navigationGroup('l4_services', 'L4 services', 'L4 service', draft.value.l4_services),
    ]
  })

  const objectOptions = computed(() =>
    navigationGroups.value.flatMap((group) =>
      group.items.map((item) => ({ ...item, group: group.label })),
    ),
  )

  function navigationGroup<T extends { name: string }>(
    collection: CollectionName,
    label: string,
    singular: string,
    items: T[],
  ): NavigationGroup {
    return {
      collection,
      label,
      singular,
      items: items.map((item, index) => ({
        key: `${collection}:${index}`,
        label: item.name || `Unnamed ${index + 1}`,
        detail: objectDetail(collection, index),
      })),
    }
  }

  function objectDetail(collection: CollectionName, index: number): string {
    if (!draft.value) return ''
    switch (collection) {
      case 'certificates':
        return `${draft.value.certificates[index]?.dns_names.length ?? 0} DNS names`
      case 'tls_profiles':
        return `${draft.value.tls_profiles[index]?.certificates.length ?? 0} identities`
      case 'listeners':
        return listenerSummary(draft.value.listeners[index])
      case 'cache_stores':
        return cacheStoreSummary(draft.value.cache_stores[index])
      case 'upstream_pools':
        return poolSummary(draft.value.upstream_pools[index])
      case 'http_services':
        return httpServiceSummary(draft.value.http_services[index])
      case 'forward_proxy_services':
        return forwardProxySummary(draft.value.forward_proxy_services[index])
      case 'rtmp_services':
        return `${draft.value.rtmp_services[index]?.applications.length ?? 0} applications`
      case 'l4_services':
        return 'TCP relay'
    }
  }

  function addObject(collection: CollectionName): void {
    if (!draft.value) return
    switch (collection) {
      case 'certificates':
        draft.value.certificates.push({
          name: '',
          dns_names: [''],
          source: { type: 'files', certificate_chain_path: '', private_key_path: '' },
        })
        break
      case 'tls_profiles':
        draft.value.tls_profiles.push({
          name: '',
          certificates: [],
          default_certificate: '',
          min_version: '1.2',
          alpn: ['http/1.1'],
          policy: {
            cipher_list: null,
            dh_parameters_path: null,
            client_auth: {
              mode: 'disabled',
              ca_certificate_path: null,
              allowed_dns_names: [],
            },
            session_cache: null,
            session_timeout_seconds: null,
            session_tickets: false,
            prefer_server_ciphers: true,
          },
        })
        break
      case 'listeners':
        draft.value.listeners.push({
          name: '',
          bind: { type: 'socket', address: '0.0.0.0:8080' },
          protocol: 'http',
          service: httpServiceNames.value[0] ?? null,
          tls_profile: null,
          max_connections: 10_000,
          downstream_timeouts: {
            client_timeout_ms: null,
            request_timeout_ms: null,
            keepalive_timeout_ms: null,
          },
        })
        break
      case 'cache_stores':
        draft.value.cache_stores.push(defaultCacheStore('memory'))
        break
      case 'upstream_pools':
        draft.value.upstream_pools.push({
          name: '',
          servers: [{
            name: 'server-1',
            endpoint: { type: 'socket', address: '127.0.0.1:3000' },
            max_connections: null,
            dns_resolution: 'on_connect',
          }],
          algorithm: 'round_robin',
          health_check: null,
          passive_health: null,
          tls: null,
          http_versions: { min: '1.1', max: '1.1' },
          queue_timeout_ms: null,
          connect_timeout_ms: null,
          server_timeout_ms: null,
          connection_reuse: 'safe',
        })
        break
      case 'http_services':
        draft.value.http_services.push({
          name: '',
          routes: [defaultHttpRoute()],
          automatic_response_headers: true,
          upstream_io_timeout_ms: 30_000,
          max_request_body_bytes: 10_485_760,
          gzip: null,
          access_log: null,
        })
        break
      case 'forward_proxy_services':
        draft.value.forward_proxy_services.push(defaultForwardProxyService())
        break
      case 'rtmp_services':
        draft.value.rtmp_services.push({
          name: '',
          outbound_chunk_size: 4_096,
          access_log: null,
          outbound_policy: defaultRtmpOutboundPolicy(),
          callbacks: defaultRtmpCallback(),
          auto_push: defaultRtmpAutoPush(),
          applications: [{
            name: '',
            live: true,
            idle_streams: true,
            publish: { rules: [], token: null },
            play: { rules: [], token: null },
            limits: {
              max_connections: 1_024,
              max_publishers: 256,
              max_viewers: 1_024,
            },
            push_targets: [],
            pull_targets: [],
            relay: defaultRtmpRelay(),
            callbacks: defaultRtmpCallback(),
            fanout: {
              max_subscribers: 1_024,
              max_queue_messages_per_subscriber: 256,
              max_queue_bytes_per_subscriber: 8_388_608,
            },
            vod: null,
            recorders: [],
          }],
        })
        break
      case 'l4_services':
        draft.value.l4_services.push({
          name: '',
          upstream_pool: '',
          connect_timeout_ms: 10_000,
          idle_timeout_ms: 300_000,
          lifetime_timeout_ms: null,
          udp: null,
        })
        break
    }
    selectedKey.value = `${collection}:${draft.value[collection].length - 1}`
    markChanged()
  }

  function removeSelected(collection: CollectionName): void {
    if (!draft.value || selection.value?.collection !== collection) return
    draft.value[collection].splice(selection.value.index, 1)
    selectedKey.value = 'general'
    markChanged()
  }

  function replaceSelectedCacheStore(store: CacheStoreConfig): void {
    if (!draft.value || selection.value?.collection !== 'cache_stores') return
    draft.value.cache_stores[selection.value.index] = store
    markChanged()
  }

  function selectionExists(key: string): boolean {
    if (key === 'general' || key === 'management' || key === 'stats') return true
    return objectOptions.value.some((option) => option.key === key)
  }

  return {
    certificateNames,
    cacheStoreNames,
    forwardProxyServices,
    httpServiceNames,
    l4PoolNames,
    l4ServiceNames,
    navigationGroups,
    objectOptions,
    poolNames,
    rtmpServiceNames,
    selectedCertificate,
    selectedCacheStore,
    selectedForwardProxyService,
    selectedHttpService,
    selectedL4Service,
    selectedListener,
    selectedPool,
    selectedRtmpService,
    selectedTlsProfile,
    selectionExists,
    tlsProfileNames,
    addObject,
    replaceSelectedCacheStore,
    removeSelected,
  }
}

function httpServiceSummary(service: CanonicalConfig['http_services'][number] | undefined): string {
  if (!service) return ''
  const count = service.routes.length
  const actions = [...new Set(service.routes.map((route) => route.action.type.replaceAll('_', ' ')))]
  return `${count} ${count === 1 ? 'route' : 'routes'}${actions.length ? ` / ${actions.join(', ')}` : ''}`
}

function listenerSummary(listener: ListenerConfig | undefined): string {
  if (!listener) return ''
  const bind = listener.bind.type === 'unix' ? listener.bind.path : listener.bind.address
  const limit = listener.max_connections === null ? 'unbounded' : `${listener.max_connections} max`
  return `${listener.protocol.toUpperCase()} / ${listener.bind.type} ${bind} / ${limit}`
}

function cacheStoreSummary(store: CacheStoreConfig | undefined): string {
  if (!store) return ''
  const capacity = store.type === 'memory' ? `${store.max_entries} entries` : `${store.max_files} files`
  return `${store.type} / ${capacity}`
}

function forwardProxySummary(
  service: CanonicalConfig['forward_proxy_services'][number] | undefined,
): string {
  if (!service) return ''
  return `${service.enabled_versions.join(', ').toUpperCase()} / ${service.connect.enabled ? 'CONNECT enabled' : 'CONNECT disabled'}`
}

function poolSummary(pool: UpstreamPoolConfig | undefined): string {
  if (!pool) return ''
  const algorithm = pool.algorithm === 'least_connections' ? 'least connections' :
    pool.algorithm === 'first' ? 'first' : 'round robin'
  return `${pool.servers.length} servers / ${algorithm}`
}

export function moveConfigurationNavigationFocus(event: KeyboardEvent): void {
  const keys = ['ArrowDown', 'ArrowRight', 'ArrowUp', 'ArrowLeft', 'Home', 'End']
  if (!keys.includes(event.key)) return
  const nav = event.currentTarget as HTMLElement
  const buttons = Array.from(nav.querySelectorAll<HTMLButtonElement>('.object-link'))
  const currentIndex = buttons.indexOf(document.activeElement as HTMLButtonElement)
  if (currentIndex < 0 || buttons.length === 0) return
  event.preventDefault()
  let nextIndex = currentIndex
  if (event.key === 'Home') nextIndex = 0
  else if (event.key === 'End') nextIndex = buttons.length - 1
  else if (event.key === 'ArrowDown' || event.key === 'ArrowRight') nextIndex += 1
  else nextIndex -= 1
  buttons[(nextIndex + buttons.length) % buttons.length]?.focus()
}
