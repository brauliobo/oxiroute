import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import ConfigurationWorkspace from './ConfigurationWorkspace.vue'
import { CANONICAL_FIELD_REGISTRY } from './config'
import { defaultRtmpCallback, defaultRtmpOutboundPolicy, defaultRtmpRelay } from './configuration/canonicalDefaults'
import { contractConfigSnapshot, jsonResponse } from './test/contractFixtures'
import type {
  CanonicalConfig,
  ConfigDiagnostic,
  ConfigSnapshot,
  ConfigValidationResponse,
} from './config'

const diskRevision = 'disk-111111111111111111111111111111111111111111111111111111111111'
const activeRevision = 'active-000000000000000000000000000000000000000000000000000000000'
const bearerToken = 'test-only-config-token'

function canonicalConfig(): CanonicalConfig {
  const expanded = contractConfigSnapshot().config
  return {
    version: 1,
    max_connections: null,
    management: { bind: '127.0.0.1:9080', ui_dir: '/opt/oxiroute/ui/dist' },
    certificates: [
      {
        name: 'direct-public',
        dns_names: ['api.example.test', '*.example.test'],
        source: {
          type: 'files',
          certificate_chain_path: '/etc/oxiroute/public.pem',
          private_key_path: '/etc/oxiroute/public-key.pem',
        },
      },
      {
        name: 'certbot-public',
        dns_names: ['certbot.example.test'],
        source: {
          type: 'certbot',
          live_directory_path: '/etc/letsencrypt/live/certbot.example.test',
          archive_directory_path: '/etc/letsencrypt/archive/certbot.example.test',
        },
      },
    ],
    tls_profiles: [
      {
        name: 'public-sni',
        certificates: ['direct-public', 'certbot-public'],
        default_certificate: 'direct-public',
        min_version: '1.2',
        alpn: ['h2', 'http/1.1'],
         policy: { cipher_list: null, dh_parameters_path: null, client_auth: { mode: 'disabled', ca_certificate_path: null, allowed_dns_names: [] }, session_cache: null, session_timeout_seconds: null, session_tickets: false, prefer_server_ciphers: true },
      },
      {
        name: 'internal-sni',
        certificates: ['direct-public'],
        default_certificate: 'direct-public',
        min_version: '1.3',
        alpn: ['http/1.1'],
         policy: { cipher_list: null, dh_parameters_path: null, client_auth: { mode: 'disabled', ca_certificate_path: null, allowed_dns_names: [] }, session_cache: null, session_timeout_seconds: null, session_tickets: false, prefer_server_ciphers: true },
      },
    ],
    listeners: [
      {
        name: 'https',
        bind: { type: 'socket', address: '0.0.0.0:443' },
        protocol: 'http',
        service: 'web',
        tls_profile: 'public-sni',
        max_connections: 10_000,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
      {
        name: 'rtmp-ingest',
        bind: { type: 'socket', address: '0.0.0.0:1935' },
        protocol: 'rtmp',
        service: 'live',
        tls_profile: null,
        max_connections: 2_000,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
      {
        name: 'database',
        bind: { type: 'unix', path: '/run/oxiroute/postgres.sock', mode: null },
        protocol: 'tcp',
        service: 'postgres',
        tls_profile: null,
        max_connections: null,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
    ],
    cache_stores: structuredClone(expanded.cache_stores),
    upstream_pools: [
      {
        name: 'web-origins',
        servers: [
          { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' },
          { name: 'server-2', endpoint: { type: 'dns', host: 'backend.example.test', port: 3001 }, max_connections: null, dns_resolution: 'on_connect' },
        ],
        algorithm: 'round_robin',
        health_check: {
          type: 'http',
          interval_ms: 5_000,
          timeout_ms: 1_000,
          healthy_threshold: 1,
          unhealthy_threshold: 3,
          startup: 'checking',
          fast_interval_ms: null,
          down_interval_ms: null,
          host: 'api.example.test',
          path: '/healthz',
          expected_status: null,
          http_version: null,
        },
        tls: null,
        http_versions: { min: '1.1', max: '1.1' },
        queue_timeout_ms: null,
        connect_timeout_ms: null,
        server_timeout_ms: null,
        connection_reuse: 'safe',
      },
      {
        name: 'secure-origins',
        servers: [{ name: 'server-1', endpoint: { type: 'socket', address: '10.0.0.20:443' }, max_connections: null, dns_resolution: 'on_connect' }],
        algorithm: 'round_robin',
        health_check: null,
        tls: {
          server_name: 'origin.example.test',
          ca_certificate_path: '/etc/oxiroute/origin-ca.pem',
        },
        http_versions: { min: '1.1', max: '2' },
        queue_timeout_ms: null,
        connect_timeout_ms: null,
        server_timeout_ms: null,
        connection_reuse: 'safe',
      },
    ],
    http_services: [
      {
        name: 'web',
        routes: [
          {
            host: { kind: 'normalized_host', value: 'api.example.test' },
            path: { kind: 'segment_prefix', value: '/v1' },
            methods: ['GET', 'POST'],
            access_policy: {
              type: 'bearer_token_file',
              token_file_path: '/run/oxiroute/api-token',
              header_name: 'authorization',
              realm: 'api',
            },
            policy: {
              max_request_body_bytes: 10_485_760,
              connect_timeout_ms: 30_000,
              read_timeout_ms: 30_000,
              write_timeout_ms: 30_000,
              request_buffering: false,
              response_buffering: false,
            },
            action: {
              type: 'proxy',
              upstream_pool: 'web-origins',
              policy: {
                upstream_host: { type: 'endpoint', unix_fallback: 'localhost' },
                request_headers: [{
                  operation: 'set',
                  name: 'x-forwarded-for',
                  value: { type: 'client_ip' },
                }],
                response_headers: [{ operation: 'remove', name: 'server' }],
                response_cookie_path_rewrites: [{ from: '/', to: '/v1' }],
                response_cookie_attributes: [],
                retry: {
                  max_retries: 1,
                  target: 'next_server',
                  delay_ms: 0,
                  final_redispatch: false,
                  triggers: ['connect_failure', 'connect_timeout', 'refused_stream'],
                  method_safety: 'get_head',
                  body_safety: 'empty',
                },
                cache: structuredClone(expanded.http_services[0]!.routes[0]!.action.type === 'proxy'
                  ? expanded.http_services[0]!.routes[0]!.action.policy.cache
                  : null),
              },
            },
          },
        ],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: 10_485_760,
        gzip: null,
        access_log: null,
      },
    ],
    forward_proxy_services: structuredClone(expanded.forward_proxy_services),
    rtmp_services: [
      {
        name: 'live',
        outbound_chunk_size: 4_096,
        access_log: null,
        outbound_policy: defaultRtmpOutboundPolicy(),
        callbacks: defaultRtmpCallback(),
        applications: [
          {
            name: 'broadcast',
            live: true,
            idle_streams: true,
            publish: { rules: [], token: null },
            play: { rules: [], token: null },
            limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
            push_targets: [],
            pull_targets: [],
            relay: defaultRtmpRelay(),
            callbacks: defaultRtmpCallback(),
            fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
            vod: null,
            recorders: [
              {
                name: 'archive',
                start: 'continuous',
                root_directory: '/var/lib/oxiroute/recordings',
                record_mask: { audio: true, video: true, keyframes: false },
                suffix_template: '-%Y-%m-%dT%H-%M-%S.flv',
                append_unix_seconds: false,
                append: false,
                lock: false,
                max_size: null,
                max_frames: null,
                notify: false,
                timezone: 'utc',
                time_basis: 'segment_start',
                segment_naming: 'safe_unique',
                rotation_interval_ms: null,
                max_queue_messages: 256,
                max_queue_bytes: 8_388_608,
                shutdown_timeout_ms: 5_000,
                max_storage_bytes: 10_737_418_240,
                max_storage_files: 10_000,
                max_active_recorders: 8,
              },
            ],
          },
        ],
      },
    ],
    l4_services: [
      {
        name: 'postgres',
        upstream_pool: 'web-origins',
        connect_timeout_ms: 10_000,
        idle_timeout_ms: 300_000,
        lifetime_timeout_ms: 3_600_000,
        udp: null,
      },
    ],
  }
}

function configSnapshot(): ConfigSnapshot {
  return {
    schemaVersion: 1,
    diskRevision,
    candidateRevision: 'candidate-1111111111111111111111111111111111111111111111111111111',
    activeRevision,
    config: canonicalConfig(),
    configFormat: 'kdl',
    compositional: false,
    dependencyCount: 0,
    configPreview: 'version 1\n',
    diagnostics: [],
  }
}

function validationResponse(config: CanonicalConfig, diagnostics: ConfigDiagnostic[] = []): ConfigValidationResponse {
  return {
    candidateRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
    normalizedConfig: structuredClone(config),
    configFormat: 'kdl',
    compositional: false,
    dependencyCount: 0,
    configPreview: `version ${config.version}\n`,
    diagnostics,
    restartRequired: false,
    topology: {
      schemaVersion: 1,
      state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1_750_000_000_000 },
      nodes: [
        {
          id: 'listener:5:https',
          kind: 'listener',
          name: config.listeners[0]?.name ?? 'https',
          configPath: '/listeners/0',
          attributes: { protocol: 'http' },
        },
      ],
      edges: [],
      overlays: [],
    },
  }
}

function deferred<T>(): {
  promise: Promise<T>
  resolve: (value: T) => void
} {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}

function findButton(wrapper: VueWrapper, text: string) {
  const button = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!button) throw new Error(`Button not found: ${text}`)
  return button
}

function findButtonIn(
  wrapper: { findAll(selector: string): Array<{ text(): string; trigger(event: string): Promise<void> }> },
  text: string,
) {
  const button = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!button) throw new Error(`Button not found: ${text}`)
  return button
}

async function unlockConfiguration(wrapper: VueWrapper, token = bearerToken): Promise<void> {
  await wrapper.get('#config-access-token').setValue(token)
  await wrapper.get('form[data-unlock-form]').trigger('submit')
  await flushPromises()
}

async function mountUnlocked(attachToBody = false): Promise<VueWrapper> {
  const wrapper = mount(
    ConfigurationWorkspace,
    attachToBody ? { attachTo: document.body } : undefined,
  )
  await unlockConfiguration(wrapper)
  return wrapper
}

function installConfigFetch(
  putResponse?: (body: CanonicalConfig) => Response,
  validationStatus = 200,
  snapshot = configSnapshot(),
  validationRestartRequired = false,
) {
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/config' && !init?.method) return jsonResponse(snapshot)
    if (url === '/api/v1/config/validate') {
      const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
      return validationStatus === 422
        ? jsonResponse({ diagnostics: [invalidDiagnostic] }, 422)
        : jsonResponse({
            ...validationResponse(body.config),
            restartRequired: validationRestartRequired,
          })
    }
    if (url === '/api/v1/config' && init?.method === 'PUT') {
      const body = JSON.parse(String(init.body)) as { config: CanonicalConfig }
      if (putResponse) return putResponse(body.config)
      return jsonResponse({
        diskRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        candidateRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        activeRevision,
        outcome: 'saved_pending_activation',
        activationState: 'pending',
        restartRequired: false,
        diagnostics: [{
          code: 'I_ACTIVATION_PENDING',
          severity: 'warning',
          stage: 'activation',
          message: 'configuration was saved and queued for generation activation',
        }],
      })
    }
    throw new Error(`Unexpected request: ${url}`)
  })
  vi.stubGlobal('fetch', fetch)
  return fetch
}

const invalidDiagnostic: ConfigDiagnostic = {
  code: 'E_UNRESOLVED_REFERENCE',
  severity: 'error',
  stage: 'validation',
  path: '/listeners/0/service',
  message: 'Listener references an unknown HTTP service.',
}

afterEach(() => {
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 })
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

describe('ConfigurationWorkspace', () => {
  it('starts locked and sends an in-memory bearer token only after accessible unlock', async () => {
    const fetch = installConfigFetch()
    const storageWrite = vi.spyOn(Storage.prototype, 'setItem')
    const wrapper = mount(ConfigurationWorkspace)
    await wrapper.vm.$nextTick()

    expect(fetch).not.toHaveBeenCalled()
    expect(wrapper.get('form[data-unlock-form]').attributes('aria-labelledby')).toBe('config-unlock-heading')
    expect(wrapper.get('label[for="config-access-token"]').text()).toContain('Bearer token')

    await wrapper.get('#config-access-token').setValue(bearerToken)
    await wrapper.get('form[data-unlock-form]').trigger('submit')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/config',
      expect.objectContaining({
        headers: expect.objectContaining({ Authorization: `Bearer ${bearerToken}` }),
      }),
    )
    expect(wrapper.find('form[data-unlock-form]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain(bearerToken)
    expect(window.location.href).not.toContain(bearerToken)
    expect(storageWrite).not.toHaveBeenCalled()
  })

  it('preserves and edits imported statistics pages and compatibility routing through save', async () => {
    const imported = configSnapshot()
    imported.config.stats = {
      binds: [],
      admin_token_file: null,
      pages: [{
        bind: '0.0.0.0:8404',
        uri_prefix: '/stats',
        refresh_ms: 10_000,
        admin: 'localhost',
        max_connections: 300,
        downstream_timeouts: {
          client_timeout_ms: 600_000,
          request_timeout_ms: 600_000,
          keepalive_timeout_ms: 60_000,
        },
      }],
    }
    const route = imported.config.http_services[0]!.routes[0]!
    route.host = {
      kind: 'ascii_case_insensitive_exact_authority',
      value: 'ollama.yellowmaverick.com',
    }
    if (route.action.type !== 'proxy') throw new Error('imported route must proxy')
    route.action.policy.retry = {
      ...route.action.policy.retry,
      max_retries: 3,
      target: 'same_server',
      delay_ms: 1_000,
      final_redispatch: true,
    }

    let saved: CanonicalConfig | undefined
    installConfigFetch((config) => {
      saved = config
      return jsonResponse({
        diskRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        candidateRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        activeRevision,
        outcome: 'saved_pending_activation',
        activationState: 'pending',
        restartRequired: false,
        diagnostics: [],
      })
    }, 200, imported)
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Statistics').trigger('click')
    const pages = wrapper.get('[data-field="stats.pages"]')
    expect(pages.text()).toContain('adds no routes beyond its URI prefix')
    expect(pages.text()).toContain('same-origin loopback requests')
    await findButtonIn(pages, 'Add statistics page').trigger('click')
    expect(wrapper.findAll('[data-field="stats.pages[].bind"]')).toHaveLength(2)
    await wrapper.get('[aria-label="Remove statistics page 2"]').trigger('click')

    await wrapper.get('[data-field="stats.pages[].bind"] input').setValue('127.0.0.1:8404')
    await wrapper.get('[data-field="stats.pages[].uri_prefix"] input').setValue('/haproxy')
    await wrapper.get('[data-field="stats.pages[].refresh_ms"] input').setValue(5_000)
    const admin = wrapper.get('[data-field="stats.pages[].admin"] select')
    await admin.setValue('disabled')
    await admin.setValue('localhost')
    await wrapper.get('[data-field="stats.pages[].max_connections"] input').setValue(250)
    await wrapper.get('[data-field="stats.pages[].downstream_timeouts.client_timeout_ms"] input')
      .setValue(30_000)
    await wrapper.get('[data-field="stats.pages[].downstream_timeouts.request_timeout_ms"] input')
      .setValue(5_000)
    await wrapper.get('[data-field="stats.pages[].downstream_timeouts.keepalive_timeout_ms"] input')
      .setValue(2_000)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(saved?.stats).toEqual({
      binds: [],
      admin_token_file: null,
      pages: [{
        bind: '127.0.0.1:8404',
        uri_prefix: '/haproxy',
        refresh_ms: 5_000,
        admin: 'localhost',
        max_connections: 250,
        downstream_timeouts: {
          client_timeout_ms: 30_000,
          request_timeout_ms: 5_000,
          keepalive_timeout_ms: 2_000,
        },
      }],
    })
    expect(saved?.http_services[0]?.routes[0]?.host).toEqual({
      kind: 'ascii_case_insensitive_exact_authority',
      value: 'ollama.yellowmaverick.com',
    })
    expect(saved?.http_services[0]?.routes[0]?.action).toMatchObject({
      policy: {
        retry: {
          max_retries: 3,
          target: 'same_server',
          delay_ms: 1_000,
          final_redispatch: true,
        },
      },
    })
  })

  it('re-locks on 401 and preserves a dirty draft for re-authentication', async () => {
    const refreshedToken = 'test-only-refreshed-token'
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(configSnapshot())
      if (url === '/api/v1/config/validate') {
        return jsonResponse({ error: { code: 'unauthorized', message: bearerToken } }, 401)
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('form[data-unlock-form]').text()).toContain('Authorization expired')
    expect(wrapper.find('.editor-form').exists()).toBe(false)
    expect(wrapper.text()).not.toContain(bearerToken)

    await unlockConfiguration(wrapper, refreshedToken)
    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('2')
    expect(wrapper.get('.revision-board').text()).toContain('Unsaved changes')
    expect(fetch.mock.calls.filter(([url]) => String(url) === '/api/v1/config')).toContainEqual([
      '/api/v1/config',
      expect.objectContaining({
        headers: { Authorization: `Bearer ${refreshedToken}` },
      }),
    ])
  })

  it('shows capability-unavailable without creating a fake editable config', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'route_not_found', message: 'route does not exist' },
    }, 404))))

    const wrapper = await mountUnlocked()

    expect(wrapper.get('.capability-panel').text()).toContain('Configuration capability unavailable')
    expect(wrapper.get('.capability-panel').text()).toContain('No placeholder configuration')
    expect(wrapper.find('.editor-form').exists()).toBe(false)
    expect(wrapper.find('[data-field="version"]').exists()).toBe(false)
  })

  it('distinguishes canonical file unavailability and renders diagnostics with revision uncertainty', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      schemaVersion: 1,
      diskRevision: null,
      activeRevision,
      diagnostics: [{
        code: 'E_CONFIG_READ',
        severity: 'error',
        stage: 'load',
        message: 'Canonical file could not be read.',
      }],
      error: {
        code: 'canonical_config_unavailable',
        message: 'the persisted canonical configuration could not be loaded',
      },
    }, 503))))

    const wrapper = await mountUnlocked()

    expect(wrapper.find('.capability-panel:not(.canonical-unavailable-panel)').exists()).toBe(false)
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('Disk revisionUnknown')
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('E_CONFIG_READ')
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('revision is uncertain')
    expect(wrapper.find('.editor-form').exists()).toBe(false)
  })

  it('does not call an unrelated 503 a missing configuration route', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'service_unavailable', message: 'temporary gateway failure' },
    }, 503))))

    const wrapper = await mountUnlocked()

    expect(wrapper.find('.capability-panel:not(.load-error-panel)').exists()).toBe(false)
    expect(wrapper.get('.load-error-panel').text()).toContain('temporary gateway failure')
  })

  it('keeps the editor visible and reports configuration refresh failures', async () => {
    let configRequests = 0
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      if (String(input) !== '/api/v1/config') throw new Error('Unexpected request')
      configRequests += 1
      return configRequests === 1
        ? jsonResponse(configSnapshot())
        : jsonResponse({ error: { code: 'config_refresh_failed', message: 'refresh failed' } }, 500)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Check disk revision').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.error').text()).toContain('Configuration refresh failed')
    expect(wrapper.get('.revision-banner.error').text()).toContain('refresh failed')
    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('2')
  })

  it('exposes disk/active distinction, both certificate sources, and plural SNI profiles', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    expect(wrapper.get('.revision-board').text()).toContain('Disk revision')
    expect(wrapper.get('.revision-board').text()).toContain('Active revision')
    expect(wrapper.get('.revision-banner.diverged').text()).toContain('serving traffic')
    expect(wrapper.text()).toContain('SNI profiles')
    expect(wrapper.text()).toContain('public-sni')
    expect(wrapper.text()).toContain('internal-sni')
    expect(wrapper.text()).toContain('memory / 100000 entries')
    expect(wrapper.text()).toContain('disk / 1000000 files')
    expect(wrapper.text()).toContain('H1, H2, H3 / CONNECT enabled')

    await findButton(wrapper, 'direct-public').trigger('click')
    expect(wrapper.get('[data-field="certificates[].source.certificate_chain_path"] input').element).toHaveProperty('value', '/etc/oxiroute/public.pem')

    await findButton(wrapper, 'certbot-public').trigger('click')
    expect(wrapper.get('[data-field="certificates[].source.live_directory_path"] input').element).toHaveProperty('value', '/etc/letsencrypt/live/certbot.example.test')
  })

  it('supports keyboard object traversal, a mobile object selector, and labeled controls', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked(true)

    const links = wrapper.findAll('.object-link')
    ;(links[0]!.element as HTMLButtonElement).focus()
    await links[0]!.trigger('keydown', { key: 'ArrowDown' })
    expect(document.activeElement).toBe(links[1]!.element)

    await wrapper.get('#mobile-object-navigation').setValue('tls_profiles:1')
    expect(wrapper.get('.form-heading').text()).toContain('internal-sni')

    for (const control of wrapper.findAll('input, select')) {
      const element = control.element as HTMLInputElement | HTMLSelectElement
      const hasWrappingLabel = element.closest('label') !== null
      const hasExplicitLabel = Boolean(element.id && document.querySelector(`label[for="${element.id}"]`))
      expect(hasWrappingLabel || hasExplicitLabel || element.hasAttribute('aria-label')).toBe(true)
    }
    wrapper.unmount()
  })

  it('exposes exactly the canonical registry through stable field controls', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    const observed = new Set<string>()
    const captureFields = () => {
      for (const field of wrapper.findAll<HTMLElement>('[data-field]')) {
        const path = field.attributes('data-field')
        expect(path).toBeDefined()
        observed.add(path!)
        expect(field.find('input, select, textarea, button').exists()).toBe(true)
      }
    }

    captureFields()
    for (const key of [
      'management',
      'certificates:0',
      'certificates:1',
      'tls_profiles:0',
      'listeners:0',
      'cache_stores:0',
      'cache_stores:1',
      'upstream_pools:0',
      'upstream_pools:1',
      'http_services:0',
      'forward_proxy_services:0',
      'rtmp_services:0',
      'l4_services:0',
    ]) {
      await wrapper.get('#mobile-object-navigation').setValue(key)
      captureFields()
      if (key === 'listeners:0') {
        await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('unix')
        captureFields()
        await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('socket')
      }
      if (key === 'upstream_pools:1') {
        await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.type"] select').setValue('unix')
        captureFields()
      }
      if (key === 'http_services:0') {
        const upstreamHost = wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.type"] select')
        await upstreamHost.setValue('literal')
        captureFields()

        const requestValue = wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.type"] select')
        await requestValue.setValue('literal')
        captureFields()

        await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].operation"] select').setValue('set')
        captureFields()

        const actionType = wrapper.get('[data-field="http_services[].routes[].action.type"] select')
        await actionType.setValue('fixed_response')
        await findButton(wrapper, 'Add header').trigger('click')
        captureFields()
        await actionType.setValue('redirect')
        captureFields()
        await actionType.setValue('static_files')
        captureFields()
      }
    }

    const registered = new Set<string>(CANONICAL_FIELD_REGISTRY.map(({ path }) => path))
    expect([...observed].every((path) => registered.has(path))).toBe(true)
    expect(observed).toContain('upstream_pools[].servers[].name')
    expect(observed).toContain('upstream_pools[].servers[].endpoint.type')
  })

  it('adds, removes, edits, and validates every canonical field control', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()
    const selectObject = (key: string) => wrapper.get('#mobile-object-navigation').setValue(key)

    await wrapper.get('[data-field="version"] input').setValue(2)

    await selectObject('management')
    const managementToggle = wrapper.get('[data-field="management"] input')
    await managementToggle.setValue(false)
    await managementToggle.setValue(true)
    await wrapper.get('[data-field="management.bind"] input').setValue('127.0.0.1:9081')
    await wrapper.get('[data-field="management.ui_dir"] input').setValue('/srv/oxiroute/ui')

    await selectObject('certificates:0')
    await wrapper.get('[data-field="certificates[].name"] input').setValue('direct-edge')
    const directDns = wrapper.get('[data-field="certificates[].dns_names"]')
    await directDns.findAll('input')[0]!.setValue('edge.example.test')
    await findButtonIn(directDns, 'Add DNS name').trigger('click')
    await directDns.findAll('input').at(-1)!.setValue('temporary.example.test')
    await directDns.findAll('.remove-row').at(-1)!.trigger('click')
    const sourceType = wrapper.get('[data-field="certificates[].source.type"] select')
    await sourceType.setValue('certbot')
    await sourceType.setValue('acme_managed')
    await wrapper.get('[data-field="certificates[].source.directory_url"] input').setValue('https://acme.test/directory')
    await wrapper.get('[data-field="certificates[].source.state_root"] input').setValue('/var/lib/oxiroute/acme')
    await wrapper.get('[data-field="certificates[].source.terms_agreed"] input').setValue(true)
    await wrapper.get('[data-field="certificates[].source.challenge"] select').setValue('dns01')
    await wrapper.get('[data-field="certificates[].source.dns01.provider"] input').setValue('fake')
    await wrapper.get('[data-field="certificates[].source.dns01.credential_file"] input').setValue('/etc/oxiroute/dns-credentials')
    await wrapper.get('[data-field="certificates[].source.dns01.timeout_seconds"] input').setValue(30)
    await wrapper.get('[data-field="certificates[].source.allowed_dns_suffixes"] .add-row').trigger('click')
    await wrapper.get('[data-field="certificates[].source.allowed_dns_suffixes"] input').setValue('example.test')
    await sourceType.setValue('self_signed_development')
    expect(wrapper.get('[data-field="certificates[].source.validity_days"] input').element).toHaveProperty('value', '7')
    await wrapper.get('[data-field="certificates[].source.validity_days"] input').setValue(14)
    await wrapper.get('[data-field="certificates[].source.key_type"] select').setValue('rsa_2048')
    await sourceType.setValue('files')
    await wrapper.get('[data-field="certificates[].source.certificate_chain_path"] input').setValue('/etc/edge-chain.pem')
    await wrapper.get('[data-field="certificates[].source.private_key_path"] input').setValue('/etc/edge-key.pem')

    await selectObject('certificates:1')
    await wrapper.get('[data-field="certificates[].name"] input').setValue('certbot-edge')
    await wrapper.get('[data-field="certificates[].dns_names"] input').setValue('certbot-edge.example.test')
    await wrapper.get('[data-field="certificates[].source.live_directory_path"] input').setValue('/etc/letsencrypt/live/edge')
    await wrapper.get('[data-field="certificates[].source.archive_directory_path"] input').setValue('/etc/letsencrypt/archive/edge')

    await selectObject('tls_profiles:0')
    expect(wrapper.get('[data-field="tls_profiles[].default_certificate"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="tls_profiles[].name"] input').setValue('edge-sni')
    const profileCertificates = wrapper.get('[data-field="tls_profiles[].certificates"]')
    await profileCertificates.findAll('input')[0]!.setValue('direct-edge')
    await profileCertificates.findAll('input')[1]!.setValue('certbot-edge')
    await findButtonIn(profileCertificates, 'Add certificate reference').trigger('click')
    await profileCertificates.findAll('.remove-row').at(-1)!.trigger('click')
    await wrapper.get('[data-field="tls_profiles[].default_certificate"] select').setValue('direct-edge')
    await wrapper.get('[data-field="tls_profiles[].min_version"] select').setValue('1.3')
    await wrapper.get('[data-field="tls_profiles[].alpn"] select').setValue('h2')
    await wrapper.get('[data-field="tls_profiles[].policy.cipher_list"] input').setValue('ECDHE-RSA-AES128-GCM-SHA256')
    await wrapper.get('[data-field="tls_profiles[].policy.dh_parameters_path"] input').setValue('/etc/letsencrypt/ssl-dhparams.pem')
    await wrapper.get('[data-field="tls_profiles[].policy.session_cache.name"] input').setValue('le_nginx_SSL')
    await wrapper.get('[data-field="tls_profiles[].policy.session_cache.size_bytes"] input').setValue(10 * 1024 * 1024)
    await wrapper.get('[data-field="tls_profiles[].policy.session_timeout_seconds"] input').setValue(86_400)
    await wrapper.get('[data-field="tls_profiles[].policy.session_tickets"] input').setValue(true)
    await wrapper.get('[data-field="tls_profiles[].policy.session_tickets"] input').setValue(false)
    await wrapper.get('[data-field="tls_profiles[].policy.prefer_server_ciphers"] input').setValue(false)
    await wrapper.get('[data-field="tls_profiles[].policy.client_auth.mode"] select').setValue('optional')
    await wrapper.get('[data-field="tls_profiles[].policy.client_auth.ca_certificate_path"] input').setValue('/etc/oxiroute/client-ca.pem')
    const allowedClientSans = wrapper.get('[data-field="tls_profiles[].policy.client_auth.allowed_dns_names"]')
    await findButtonIn(allowedClientSans, 'Add exact DNS or IP SAN').trigger('click')
    await allowedClientSans.find('input').setValue('client.example.test')

    await selectObject('listeners:0')
    await wrapper.get('[data-field="listeners[].name"] input').setValue('edge-https')
    const bindType = wrapper.get('[data-field="listeners[].bind.type"] select')
    await bindType.setValue('unix')
    await wrapper.get('[data-field="listeners[].bind.path"] input').setValue('/run/oxiroute/edge.sock')
    await bindType.setValue('socket')
    await wrapper.get('[data-field="listeners[].bind.address"] input').setValue('127.0.0.1:8443')
    const listenerProtocol = wrapper.get('[data-field="listeners[].protocol"] select')
    await listenerProtocol.setValue('tcp')
    await listenerProtocol.setValue('http')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('web')
    await wrapper.get('[data-field="listeners[].tls_profile"] select').setValue('edge-sni')
    await wrapper.get('[data-field="listeners[].max_connections"] input').setValue(12_000)

    await selectObject('upstream_pools:0')
    await wrapper.get('[data-field="upstream_pools[].name"] input').setValue('origins-a')
    const servers = wrapper.get('[data-field="upstream_pools[].servers"]')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.address"] input').setValue('127.0.0.1:3100')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.host"] input').setValue('edge-backend.example.test')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.port"] input').setValue(3101)
    await findButtonIn(servers, 'Add server').trigger('click')
    await wrapper.get('[aria-label="Remove upstream server 3"]').trigger('click')
    await wrapper.get('[data-field="upstream_pools[].algorithm"] select').setValue('least_connections')
    const health = wrapper.get('[data-field="upstream_pools[].health_check"]')
    const healthToggle = health.find('.enable-row input')
    await healthToggle.setValue(false)
    await healthToggle.setValue(true)
    const healthType = wrapper.get('[data-field="upstream_pools[].health_check.type"] select')
    await healthType.setValue('http')
    await wrapper.get('[data-field="upstream_pools[].health_check.interval_ms"] input').setValue(6_000)
    await wrapper.get('[data-field="upstream_pools[].health_check.timeout_ms"] input').setValue(1_500)
    await wrapper.get('[data-field="upstream_pools[].health_check.healthy_threshold"] input').setValue(2)
    await wrapper.get('[data-field="upstream_pools[].health_check.unhealthy_threshold"] input').setValue(4)
    await wrapper.get('[data-field="upstream_pools[].health_check.host"] input').setValue('edge.example.test')
    await wrapper.get('[data-field="upstream_pools[].health_check.path"] input').setValue('/ready')
    await wrapper.get('[data-field="upstream_pools[].http_versions.min"] select').setValue('1.1')
    expect(wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]').attributes()).toHaveProperty('disabled')

    await selectObject('upstream_pools:1')
    const upstreamTls = wrapper.get('[data-field="upstream_pools[].tls"]')
    const upstreamTlsToggle = upstreamTls.find('.enable-row input')
    await upstreamTlsToggle.setValue(false)
    await upstreamTlsToggle.setValue(true)
    await wrapper.get('[data-field="upstream_pools[].tls.server_name"] input').setValue('secure-edge.example.test')
    await wrapper.get('[data-field="upstream_pools[].tls.ca_certificate_path"] input').setValue('/etc/secure-edge-ca.pem')
    await wrapper.get('[data-field="upstream_pools[].http_versions.max"] select').setValue('2')

    await selectObject('http_services:0')
    await wrapper.get('[data-field="http_services[].name"] input').setValue('web-edge')
    await wrapper.get('[data-field="http_services[].automatic_response_headers"] input').setValue(false)
    await wrapper.get('[data-field="http_services[].upstream_io_timeout_ms"] input').setValue(31_000)
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] select').setValue('unbounded')
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] select').setValue('bounded')
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] input').setValue(20_000_000)
    await wrapper.get('[data-field="http_services[].routes[].host.kind"] select').setValue('exact_authority')
    await wrapper.get('[data-field="http_services[].routes[].host.value"] input').setValue('edge.example.test:8443')
    await wrapper.get('[data-field="http_services[].routes[].path.kind"] select').setValue('exact')
    await wrapper.get('[data-field="http_services[].routes[].path.value"] input').setValue('/edge')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.token_file_path"] input').setValue('/run/oxiroute/edge-token')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.header_name"] input').setValue('x-api-token')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.realm"] input').setValue('edge')
    expect(wrapper.get('[data-field="http_services[].routes[].action.upstream_pool"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="http_services[].routes[].action.upstream_pool"] select').setValue('origins-a')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.type"] select').setValue('literal')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.value"] input').setValue('origin.example.test')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].name"] input').setValue('x-client')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.type"] select').setValue('literal')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.value"] input').setValue('edge-client')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].operation"] select').setValue('set')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].name"] input').setValue('x-edge')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].value"] input').setValue('active')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].from"] input').setValue('/v1')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].to"] input').setValue('/edge')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.retry.max_retries"] input').setValue(2)
    const methods = wrapper.get('[data-field="http_services[].routes[].methods"]')
    await methods.findAll('input')[0]!.setValue('PUT')
    await findButtonIn(methods, 'Add method').trigger('click')
    await methods.findAll('.remove-row').at(-1)!.trigger('click')
    await findButton(wrapper, 'Add route').trigger('click')
    await wrapper.get('[aria-label="Remove route 2"]').trigger('click')

    await selectObject('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].name"] input').setValue('media-edge')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('publish')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].live"] input').attributes())
      .toHaveProperty('disabled')
    await wrapper.get('[data-field="rtmp_services[].applications[].idle_streams"] input').setValue(false)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].name"] input').setValue('manual-archive')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].start"] select').setValue('manual')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].root_directory"] input').setValue('/srv/recordings')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].suffix_template"] input').setValue('-%Y%m%d.flv')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].append_unix_seconds"] input').setValue(true)
    const rotation = wrapper.get('[data-field="rtmp_services[].applications[].recorders[].rotation_interval_ms"]')
    await rotation.get('select').setValue('bounded')
    await rotation.get('input').setValue(120_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_queue_messages"] input').setValue(512)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_queue_bytes"] input').setValue(16_777_216)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].shutdown_timeout_ms"] input').setValue(6_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_storage_bytes"] input').setValue(21_474_836_480)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_storage_files"] input').setValue(20_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_active_recorders"] input').setValue(16)
    await findButton(wrapper, 'Add recorder').trigger('click')
    await wrapper.get('[aria-label="Remove recorder 2"]').trigger('click')
    await findButton(wrapper, 'Add application').trigger('click')
    await wrapper.get('[aria-label="Remove RTMP application 2"]').trigger('click')

    await selectObject('l4_services:0')
    await wrapper.get('[data-field="l4_services[].name"] input').setValue('database-edge')
    expect(wrapper.get('[data-field="l4_services[].upstream_pool"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="l4_services[].upstream_pool"] select').setValue('origins-a')
    await wrapper.get('[data-field="l4_services[].connect_timeout_ms"] input').setValue(11_000)
    await wrapper.get('[data-field="l4_services[].idle_timeout_ms"] input').setValue(301_000)
    await wrapper.get('[data-field="l4_services[].lifetime_timeout_ms"] input').setValue(3_601_000)

    for (const [addLabel, removeLabel] of [
      ['Add certificate', 'Remove certificate'],
      ['Add SNI profile', 'Remove profile'],
      ['Add listener', 'Remove listener'],
      ['Add upstream pool', 'Remove pool'],
      ['Add HTTP service', 'Remove service'],
      ['Add RTMP service', 'Remove service'],
      ['Add L4 service', 'Remove service'],
    ] as const) {
      await wrapper.get(`[aria-label="${addLabel}"]`).trigger('click')
      await findButton(wrapper, removeLabel).trigger('click')
    }

    await selectObject('listeners:0')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('web-edge')
    await selectObject('listeners:1')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('media-edge')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted).toEqual(expect.objectContaining({
      version: 2,
      management: { bind: '127.0.0.1:9081', ui_dir: '/srv/oxiroute/ui' },
    }))
    expect(submitted.certificates[0]).toEqual(expect.objectContaining({
      name: 'direct-edge',
      source: { type: 'files', certificate_chain_path: '/etc/edge-chain.pem', private_key_path: '/etc/edge-key.pem' },
    }))
    expect(submitted.tls_profiles[0]).toEqual(expect.objectContaining({
      name: 'edge-sni',
      certificates: ['direct-edge', 'certbot-edge'],
      default_certificate: 'direct-edge',
      min_version: '1.3',
      alpn: ['h2'],
      policy: {
         cipher_list: 'ECDHE-RSA-AES128-GCM-SHA256',
         dh_parameters_path: '/etc/letsencrypt/ssl-dhparams.pem',
         client_auth: {
           mode: 'optional',
           ca_certificate_path: '/etc/oxiroute/client-ca.pem',
           allowed_dns_names: ['client.example.test'],
         },
         session_cache: { name: 'le_nginx_SSL', size_bytes: 10 * 1024 * 1024 },
        session_timeout_seconds: 86_400,
        session_tickets: false,
        prefer_server_ciphers: false,
      },
    }))
    expect(submitted.upstream_pools[0]).toEqual(expect.objectContaining({
      name: 'origins-a',
      servers: [
        { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3100' }, max_connections: null, dns_resolution: 'on_connect' },
        { name: 'server-2', endpoint: { type: 'dns', host: 'edge-backend.example.test', port: 3101 }, max_connections: null, dns_resolution: 'on_connect' },
      ],
      algorithm: 'least_connections',
      health_check: expect.objectContaining({ host: 'edge.example.test', path: '/ready' }),
      http_versions: { min: '1.1', max: '1.1' },
    }))
    expect(submitted.http_services[0]).toEqual(expect.objectContaining({
      name: 'web-edge',
      automatic_response_headers: false,
      upstream_io_timeout_ms: 31_000,
      max_request_body_bytes: 20_000_000,
    }))
    expect(submitted.http_services[0]?.routes[0]).toEqual({
      host: { kind: 'exact_authority', value: 'edge.example.test:8443' },
      path: { kind: 'exact', value: '/edge' },
      methods: ['PUT', 'POST'],
      access_policy: {
        type: 'bearer_token_file',
        token_file_path: '/run/oxiroute/edge-token',
        header_name: 'x-api-token',
        realm: 'edge',
      },
      policy: {
        max_request_body_bytes: 10_485_760,
        connect_timeout_ms: 30_000,
        read_timeout_ms: 30_000,
        write_timeout_ms: 30_000,
        request_buffering: false,
        response_buffering: false,
      },
      action: {
        type: 'proxy',
        upstream_pool: 'origins-a',
        policy: {
          upstream_host: { type: 'literal', value: 'origin.example.test' },
          request_headers: [{
            operation: 'set',
            name: 'x-client',
            value: { type: 'literal', value: 'edge-client' },
          }],
          response_headers: [{ operation: 'set', name: 'x-edge', value: 'active', always: true }],
          response_cookie_path_rewrites: [{ from: '/v1', to: '/edge' }],
          response_cookie_attributes: [],
          retry: {
            max_retries: 2,
            target: 'next_server',
            delay_ms: 0,
            final_redispatch: false,
            triggers: ['connect_failure', 'connect_timeout', 'refused_stream'],
            method_safety: 'get_head',
            body_safety: 'empty',
          },
          cache: expect.objectContaining({ store: 'responses' }),
        },
      },
    })
    expect(submitted.rtmp_services[0]).toEqual({
      name: 'media-edge',
      outbound_chunk_size: 4_096,
      access_log: null,
      outbound_policy: defaultRtmpOutboundPolicy(),
      callbacks: defaultRtmpCallback(),
      applications: [{
        name: 'publish',
        live: true,
        idle_streams: false,
        publish: { rules: [], token: null },
        play: { rules: [], token: null },
        limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
        push_targets: [],
        pull_targets: [],
        relay: defaultRtmpRelay(),
        callbacks: defaultRtmpCallback(),
        fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
        vod: null,
        recorders: [{
          name: 'manual-archive',
          start: 'manual',
          root_directory: '/srv/recordings',
          record_mask: { audio: true, video: true, keyframes: false },
          suffix_template: '-%Y%m%d.flv',
          append_unix_seconds: true,
          append: false,
          lock: false,
          max_size: null,
          max_frames: null,
          notify: false,
          timezone: 'utc',
          time_basis: 'segment_start',
          segment_naming: 'safe_unique',
          rotation_interval_ms: 120_000,
          max_queue_messages: 512,
          max_queue_bytes: 16_777_216,
          shutdown_timeout_ms: 6_000,
          max_storage_bytes: 21_474_836_480,
          max_storage_files: 20_000,
          max_active_recorders: 16,
        }],
      }],
    })
    expect(submitted.l4_services[0]).toEqual({
      name: 'database-edge',
      upstream_pool: 'origins-a',
      connect_timeout_ms: 11_000,
      idle_timeout_ms: 301_000,
      lifetime_timeout_ms: 3_601_000,
      udp: null,
    })
  })

  it('edits RTMP listeners and every RTMP service field with protocol-aware references', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:1')
    const service = wrapper.get('[data-field="listeners[].service"] select')
    expect(service.findAll('option').map((option) => option.text())).toEqual(['Select a service', 'live'])
    expect(service.text()).not.toContain('web')
    expect(service.text()).not.toContain('postgres')

    await wrapper.get('[data-field="listeners[].bind.address"] input').setValue('127.0.0.1:1936')
    await wrapper.get('[data-field="listeners[].protocol"] select').setValue('http')
    expect((service.element as HTMLSelectElement).value).toBe('web')
    expect(service.text()).toContain('web')
    expect(service.text()).not.toContain('live')
    await wrapper.get('[data-field="listeners[].protocol"] select').setValue('rtmp')
    expect((service.element as HTMLSelectElement).value).toBe('live')

    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].name"] input').setValue('live-edge')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('camera')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].live"] input').attributes())
      .toHaveProperty('disabled')
    await wrapper.get('[data-field="rtmp_services[].applications[].idle_streams"] input').setValue(false)

    await findButton(wrapper, 'Add application').trigger('click')
    expect(wrapper.findAll('[data-field="rtmp_services[].applications[].name"]')).toHaveLength(2)
    await wrapper.get('[aria-label="Remove RTMP application 2"]').trigger('click')
    expect(wrapper.findAll('[data-field="rtmp_services[].applications[].name"]')).toHaveLength(1)

    await wrapper.get('[aria-label="Add RTMP service"]').trigger('click')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].name"] input').element).toBeInstanceOf(HTMLInputElement)
    await findButton(wrapper, 'Remove service').trigger('click')
    await wrapper.get('[aria-label="Add listener"]').trigger('click')
    await findButton(wrapper, 'Remove listener').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('listeners:1')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('live-edge')
    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.listeners[1]).toEqual(expect.objectContaining({
      bind: { type: 'socket', address: '127.0.0.1:1936' },
      protocol: 'rtmp',
      service: 'live-edge',
      tls_profile: null,
    }))
    expect(submitted.rtmp_services).toEqual([
      {
        name: 'live-edge',
        outbound_chunk_size: 4_096,
        access_log: null,
        outbound_policy: defaultRtmpOutboundPolicy(),
        callbacks: defaultRtmpCallback(),
        applications: [{
          name: 'camera',
          live: true,
          idle_streams: false,
          publish: { rules: [], token: null },
          play: { rules: [], token: null },
          limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
          push_targets: [],
          pull_targets: [],
          relay: defaultRtmpRelay(),
          callbacks: defaultRtmpCallback(),
          fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
          vod: null,
          recorders: canonicalConfig().rtmp_services[0]!.applications[0]!.recorders,
        }],
      },
    ])
  })

  it('serializes explicit unbounded limits as null and never substitutes zero', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')
    const listenerLimit = wrapper.get('[data-field="listeners[].max_connections"]')
    await listenerLimit.get('input').setValue(0)
    expect((listenerLimit.get('input').element as HTMLInputElement).value).toBe('10000')
    await listenerLimit.get('select').setValue('unbounded')

    await wrapper.get('#mobile-object-navigation').setValue('http_services:0')
    const bodyLimit = wrapper.get('[data-field="http_services[].max_request_body_bytes"]')
    await bodyLimit.get('input').setValue(0)
    expect((bodyLimit.get('input').element as HTMLInputElement).value).toBe('10485760')
    await bodyLimit.get('select').setValue('unbounded')

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.listeners[0]).toHaveProperty('max_connections', null)
    expect(submitted.http_services[0]).toHaveProperty('max_request_body_bytes', null)
  })

  it('prevents invalid protocol, TLS, health, HTTP/2, and L4 pool combinations', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:2')
    const tcpService = wrapper.get('[data-field="listeners[].service"] select')
    expect(tcpService.attributes()).toHaveProperty('required')
    expect(tcpService.findAll('option').map((option) => option.text())).toEqual([
      'Select a service',
      'postgres',
    ])
    expect(wrapper.get('[data-field="listeners[].tls_profile"] select').attributes()).toHaveProperty('disabled')
    expect((wrapper.get('[data-field="listeners[].max_connections"] select').element as HTMLSelectElement).value).toBe('unbounded')

    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')
    const listenerTls = wrapper.get('[data-field="listeners[].tls_profile"] select')
    expect((listenerTls.element as HTMLSelectElement).value).toBe('public-sni')
    await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('unix')
    const unixMode = wrapper.get('[data-field="listeners[].bind.mode"] input')
    await unixMode.setValue('0777')
    await unixMode.trigger('change')
    expect((wrapper.vm as unknown as { draft: CanonicalConfig }).draft.listeners[0]?.bind)
      .toEqual({ type: 'unix', path: '', mode: 0o777 })
    expect(listenerTls.attributes()).toHaveProperty('disabled')
    expect((listenerTls.element as HTMLSelectElement).selectedOptions[0]?.text).toBe('None')

    await wrapper.get('#mobile-object-navigation').setValue('upstream_pools:0')
    const tlsToggle = wrapper.get('[data-field="upstream_pools[].tls"] input')
    const h2Option = wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]')
    expect(tlsToggle.attributes()).toHaveProperty('disabled')
    expect(h2Option.attributes()).toHaveProperty('disabled')
    const endpointTypes = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.type"] select')
    await endpointTypes[1]!.setValue('unix')
    expect(wrapper.get('[data-field="upstream_pools[].health_check"] input').attributes()).toHaveProperty('disabled')
    expect(wrapper.get('[data-field="upstream_pools[].tls"] input').attributes()).toHaveProperty('disabled')
    expect((wrapper.get('[data-field="upstream_pools[].health_check"] input').element as HTMLInputElement).checked).toBe(false)
    await endpointTypes[1]!.setValue('dns')
    await tlsToggle.setValue(true)
    expect(wrapper.get('[data-field="upstream_pools[].health_check"] input').attributes()).toHaveProperty('disabled')
    expect(wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]').attributes('disabled')).toBeUndefined()

    await wrapper.get('#mobile-object-navigation').setValue('l4_services:0')
    const l4Pool = wrapper.get('[data-field="l4_services[].upstream_pool"] select')
    expect(l4Pool.findAll('option').map((option) => option.text())).not.toContain('secure-origins')
    expect((l4Pool.element as HTMLSelectElement).value).toBe('')
  })

  it('supports a complete mobile configuration edit and validation flow', async () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    const mobileAdds = wrapper.findAll('.mobile-add')
    expect(mobileAdds).toHaveLength(9)
    expect(mobileAdds.map((button) => button.attributes('aria-label'))).toEqual([
      'Add certificate',
      'Add SNI profile',
      'Add listener',
      'Add cache store',
      'Add upstream pool',
      'Add HTTP service',
      'Add forward proxy',
      'Add RTMP service',
      'Add L4 service',
    ])
    expect(mobileAdds.every((button) => button.attributes('type') === 'button')).toBe(true)
    await mobileAdds[0]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed certificate')
    await findButton(wrapper, 'Remove certificate').trigger('click')

    await mobileAdds[3]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed cache store')
    await findButton(wrapper, 'Remove cache store').trigger('click')

    await mobileAdds[6]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed forward proxy')
    await findButton(wrapper, 'Remove forward proxy').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('upstream_pools:0')
    await findButton(wrapper, 'Add server').trigger('click')
    const endpointTypes = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.type"] select')
    await endpointTypes.at(-1)!.setValue('unix')
    await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.path"] input').setValue('/run/oxiroute/mobile.sock')
    await wrapper.get('[aria-label="Remove endpoint 3"]').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('mobile-live')
    await findButton(wrapper, 'Add application').trigger('click')
    const names = wrapper.findAll('[data-field="rtmp_services[].applications[].name"] input')
    await names[1]!.setValue('mobile-backup')
    const recorderLists = wrapper.findAll('[data-field="rtmp_services[].applications[].recorders"]')
    await findButtonIn(recorderLists[1]!, 'Add recorder').trigger('click')
    await findButtonIn(recorderLists[1]!, 'Add recorder').trigger('click')
    const recorderNames = recorderLists[1]!.findAll('[data-field="rtmp_services[].applications[].recorders[].name"] input')
    await recorderNames[1]!.setValue('mobile-manual')
    await recorderLists[1]!.findAll('[data-field="rtmp_services[].applications[].recorders[].start"] select')[1]!.setValue('manual')
    await recorderLists[1]!.get('[aria-label="Remove recorder 1"]').trigger('click')
    await wrapper.get('[aria-label="Remove RTMP application 1"]').trigger('click')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.rtmp_services[0]?.applications).toEqual([
      expect.objectContaining({
        name: 'mobile-backup',
        live: true,
        idle_streams: true,
        recorders: [expect.objectContaining({ name: 'mobile-manual', start: 'manual' })],
      }),
    ])
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
  })

  it('edits fixed, redirect, and static actions through the authenticated mobile workspace', async () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()
    await wrapper.get('#mobile-object-navigation').setValue('http_services:0')
    const actionType = () => wrapper.get('[data-field="http_services[].routes[].action.type"] select')

    await actionType().setValue('fixed_response')
    await wrapper.get('[data-field="http_services[].routes[].action.status"] input').setValue(201)
    await wrapper.get('[data-field="http_services[].routes[].action.body"] textarea').setValue('created')

    await actionType().setValue('redirect')
    await wrapper.get('[data-field="http_services[].routes[].action.location.kind"] select').setValue('request_template')
    await wrapper.get('[data-field="http_services[].routes[].action.location.value"] input').setValue('https://$host$request_uri')

    await actionType().setValue('static_files')
    await wrapper.get('[data-field="http_services[].routes[].action.root_directory"] input').setValue('/srv/mobile-site')
    await wrapper.get('[data-field="http_services[].routes[].action.spa_fallback"] input').setValue('app.html')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.http_services[0]?.routes[0]?.action).toEqual({
      type: 'static_files',
      root_directory: '/srv/mobile-site',
      path_mapping: 'root',
      index_files: ['index.html'],
      internal_index_redirects: false,
      directory_redirects: false,
      spa_fallback: 'app.html',
      try_files: [],
      autoindex: false,
      autoindex_exact_size: true,
      autoindex_local_time: false,
      etag: true,
      mime: { default_type: null, types: [] },
      headers: [],
      error_responses: [],
    })
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
  })

  it('guards browser unload and explicit draft reset', async () => {
    installConfigFetch()
    const confirm = vi.fn(() => false)
    vi.stubGlobal('confirm', confirm)
    const wrapper = await mountUnlocked()

    const version = wrapper.get('[data-field="version"] input')
    await version.setValue(2)
    const unload = new Event('beforeunload', { cancelable: true })
    expect(window.dispatchEvent(unload)).toBe(false)
    expect(unload.defaultPrevented).toBe(true)

    await findButton(wrapper, 'Reset draft').trigger('click')
    expect(confirm).toHaveBeenCalledOnce()
    expect((version.element as HTMLInputElement).value).toBe('2')

    confirm.mockReturnValue(true)
    await findButton(wrapper, 'Reset draft').trigger('click')
    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('1')
  })

  it('keeps compositional roots read-only while preserving inspection and validation', async () => {
    const compositionalSnapshot = {
      ...configSnapshot(),
      configFormat: 'hocon' as const,
      compositional: true,
      dependencyCount: 2,
      configPreview: '{ version: 1 }',
    }
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(compositionalSnapshot)
      if (url === '/api/v1/config/validate') {
        const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
        return jsonResponse({
          ...validationResponse(body.config),
          configFormat: 'hocon',
          compositional: true,
          dependencyCount: 2,
          configPreview: '{ version: 1 }',
        })
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    expect(wrapper.get('.revision-banner.compositional').text()).toContain('Compositional root is read-only')
    expect(wrapper.get('.revision-banner.compositional').text()).toContain('2 dependencies')
    expect(wrapper.get('.editor-form').attributes()).toMatchObject({ inert: '', 'aria-readonly': 'true' })
    expect(wrapper.findAll('.mobile-add').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
    expect(wrapper.findAll('.nav-add').every((button) => button.attributes('disabled') !== undefined)).toBe(true)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.preview-panel').text()).toContain('HOCON configuration preview')
    expect(wrapper.get('.preview-panel pre').text()).toContain('{ version: 1 }')
    const reviewButton = findButton(wrapper, 'Review save')
    expect((reviewButton.element as HTMLButtonElement).disabled).toBe(true)
    expect(reviewButton.attributes('title')).toContain('cannot replace a compositional configuration root')
    expect(fetch.mock.calls.some(([, init]) => init?.method === 'PUT')).toBe(false)
  })

  it('validates, presents the format-neutral preview, and saves with If-Config-Revision', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked(true)

    await findButton(wrapper, 'https').trigger('click')
    await wrapper.get('[data-field="listeners[].name"] input').setValue('public-https')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    expect(validationCall?.[1]).toEqual(expect.objectContaining({
      method: 'POST',
      headers: expect.objectContaining({ Authorization: `Bearer ${bearerToken}` }),
    }))
    expect(JSON.parse(String(validationCall?.[1]?.body)).config.listeners[0].name).toBe('public-https')
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
    expect(wrapper.get('.preview-panel pre').text()).toContain('version 1')
    expect(wrapper.get('.candidate-topology').text()).toContain('public-https')
    expect(wrapper.get('.candidate-state').text()).toContain('Candidate only / not active')

    const reviewButton = findButton(wrapper, 'Review save')
    ;(reviewButton.element as HTMLButtonElement).focus()
    await reviewButton.trigger('click')
    expect(wrapper.get('[role="dialog"]').text()).toContain('Save review')
    expect(wrapper.get('.review-warning').text()).toContain('no process restart is required')
    expect(document.activeElement).toBe(wrapper.get('.save-review .close-button').element)
    expect(wrapper.get('.config-layout').attributes()).toHaveProperty('inert')
    await wrapper.get('[role="dialog"]').trigger('keydown', { key: 'Escape' })
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    expect(document.activeElement).toBe(reviewButton.element)

    await reviewButton.trigger('click')
    await wrapper.get('.review-scrim').trigger('click')
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    expect(document.activeElement).toBe(reviewButton.element)

    await reviewButton.trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    const saveCall = fetch.mock.calls.find(([, init]) => init?.method === 'PUT')
    expect(saveCall?.[1]?.headers).toEqual(expect.objectContaining({
      Authorization: `Bearer ${bearerToken}`,
      'If-Config-Revision': diskRevision,
    }))
    const saved = JSON.parse(String(saveCall?.[1]?.body)).config as CanonicalConfig
    expect(saved.listeners[0]?.bind).toEqual({ type: 'socket', address: '0.0.0.0:443' })
    expect(saved.listeners[2]).toHaveProperty('max_connections', null)
    expect(saved.upstream_pools[0]?.servers).toEqual([
      { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' },
      { name: 'server-2', endpoint: { type: 'dns', host: 'backend.example.test', port: 3001 }, max_connections: null, dns_resolution: 'on_connect' },
    ])
    expect(saved.http_services[0]).toHaveProperty('max_request_body_bytes', 10_485_760)
    expect(wrapper.get('.revision-banner.pending').text()).toContain('activation pending')
    expect(wrapper.get('.revision-banner.pending').text()).toContain('no restart is required')
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    expect(document.activeElement).toBe(wrapper.get('.revision-banner.save-state').element)
    wrapper.unmount()
  })

  it('renders an exact unchanged-active 200 outcome without requiring restart', async () => {
    installConfigFetch(() => jsonResponse({
      diskRevision,
      candidateRevision: 'candidate-unchanged',
      activeRevision,
      outcome: 'unchanged_active',
      activationState: 'active',
      restartRequired: false,
      diagnostics: [],
    }))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    expect(wrapper.get('.review-warning').text()).toContain('no process restart is required')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.success').text()).toContain('unchanged')
    expect(wrapper.get('.revision-banner.success').text()).toContain('No restart is required')
    expect(wrapper.text()).not.toContain('activation pending')
  })

  it('renders the explicit restart-required outcome for an active Unix mode change', async () => {
    installConfigFetch(() => jsonResponse({
      diskRevision: 'disk-restart-required',
      candidateRevision: 'candidate-restart-required',
      activeRevision,
      outcome: 'saved_restart_required',
      activationState: 'restart_required',
      restartRequired: true,
      diagnostics: [{
        code: 'I_RESTART_REQUIRED',
        severity: 'warning',
        stage: 'activation',
        message: 'an active Unix listener mode changed',
      }],
    }), 200, configSnapshot(), true)
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    expect(wrapper.get('.review-warning').text()).toContain('next process restart')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.pending').text()).toContain('restart required')
    expect(wrapper.get('.revision-banner.pending').text()).toContain('active Unix listener mode')
  })

  it('preserves the edited draft after a 409 conflict', async () => {
    installConfigFetch(() => jsonResponse({
      schemaVersion: 1,
      diskRevision: 'disk-newer-revision',
      activeRevision,
      expectedRevision: diskRevision,
      outcome: 'conflict',
      config: canonicalConfig(),
      diagnostics: [],
    }, 409))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'https').trigger('click')
    const name = wrapper.get('[data-field="listeners[].name"] input')
    await name.setValue('preserved-listener')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.stale').text()).toContain('Draft preserved')
    expect(wrapper.get('.revision-banner.error').text()).toContain('Save conflict')
    expect((wrapper.get('[data-field="listeners[].name"] input').element as HTMLInputElement).value).toBe('preserved-listener')
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)

    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    await findButton(wrapper, 'Reset draft').trigger('click')
    expect(wrapper.get('.revision-banner.stale').element).toBeInstanceOf(HTMLElement)
    expect((wrapper.get('[data-field="listeners[].name"] input').element as HTMLInputElement).value).toBe('preserved-listener')
    confirm.mockRestore()
  })

  it('renders 422 validation diagnostics and keeps review blocked', async () => {
    installConfigFetch(undefined, 422)
    const wrapper = await mountUnlocked(true)

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.diagnostic-list').text()).toContain('E_UNRESOLVED_REFERENCE')
    expect(wrapper.get('.diagnostic-list').text()).toContain('Listener references an unknown HTTP service')
    expect(wrapper.get('.revision-banner.error').text()).toContain('Candidate is invalid')
    expect((findButton(wrapper, 'Review save').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('aborts and generation-gates superseded validation success and errors', async () => {
    const firstValidation = deferred<Response>()
    const validationSignals: AbortSignal[] = []
    let validationRequests = 0
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config') return jsonResponse(configSnapshot())
      if (url === '/api/v1/config/validate') {
        validationRequests += 1
        validationSignals.push(init?.signal as AbortSignal)
        if (validationRequests === 1) return firstValidation.promise
        const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
        return jsonResponse(validationResponse(body.config))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await wrapper.get('[data-field="version"] input').setValue(3)
    expect(validationSignals[0]?.aborted).toBe(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    firstValidation.resolve(jsonResponse({ diagnostics: [invalidDiagnostic] }, 422))
    await flushPromises()

    expect(wrapper.get('.preview-panel pre').text()).toContain('version 3')
    expect(wrapper.text()).not.toContain('Candidate is invalid')
    expect(wrapper.find('.diagnostic-list').exists()).toBe(false)
  })

  it('aborts validation when the workspace unmounts', async () => {
    const pendingValidation = deferred<Response>()
    let validationSignal: AbortSignal | undefined
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config') return jsonResponse(configSnapshot())
      if (url === '/api/v1/config/validate') {
        validationSignal = init?.signal as AbortSignal
        return pendingValidation.promise
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    wrapper.unmount()

    expect(validationSignal?.aborted).toBe(true)
  })

  it('keeps save-time 422 diagnostics attached to the rejected candidate', async () => {
    installConfigFetch(() => jsonResponse({
      diagnostics: [invalidDiagnostic],
    }, 422))
    const wrapper = await mountUnlocked(true)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.error').text()).toContain('Save rejected as invalid')
    expect(wrapper.get('.diagnostic-list').text()).toContain('E_UNRESOLVED_REFERENCE')
    expect(wrapper.find('.output-empty').exists()).toBe(false)
    expect(document.activeElement).toBe(wrapper.get('#diagnostics-heading').element)
  })

  it('keeps the review dialog visible for a 428 revision precondition failure', async () => {
    installConfigFetch(() => jsonResponse({
      error: { code: 'precondition_required', message: 'configuration revision required' },
    }, 428))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('[role="dialog"]').text()).toContain('revision precondition')
    expect(wrapper.get('.revision-banner.stale').element).toBeInstanceOf(HTMLElement)
    expect(wrapper.get('[role="dialog"] .primary-button').attributes()).toHaveProperty('disabled')
  })

  it('blocks writes when the server cannot reload authoritative state after a conflict', async () => {
    installConfigFetch(() => jsonResponse({
      schemaVersion: 1,
      diskRevision: null,
      activeRevision,
      expectedRevision: diskRevision,
      outcome: 'authoritative_state_unavailable',
      diagnostics: [{
        code: 'E_CONFIG_READ',
        severity: 'error',
        stage: 'read',
        message: 'canonical configuration could not be read',
      }],
      error: {
        code: 'authoritative_config_unavailable',
        message: 'the latest persisted configuration could not be loaded',
      },
    }, 503))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.stale').text()).toContain('Draft preserved')
    expect(wrapper.get('.revision-banner.error').text()).toContain('Authoritative configuration unavailable')
    expect(wrapper.get('.diagnostic-list').text()).toContain('E_CONFIG_READ')
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
  })

  it.each([
    {
      outcome: 'write_failed',
      diskRevision,
      code: 'E_CONFIG_TEMP_WRITE',
      expected: 'canonical write failed',
    },
  ] as const)('keeps 500 $outcome details visible in the review dialog', async ({
    outcome,
    diskRevision: failureRevision,
    code,
    expected,
  }) => {
    installConfigFetch(() => jsonResponse({
      diskRevision: failureRevision,
      activeRevision,
      outcome,
      diagnostics: [{
        code,
        severity: 'error',
        stage: 'write',
        message: 'The canonical persistence operation failed.',
      }],
    }, 500))
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('[role="dialog"] .dialog-error').text()).toContain(expected)
    expect(wrapper.get('.diagnostic-list').text()).toContain(code)
    expect(wrapper.get('.revision-board').text()).toContain('Unsaved changes')
    expect(wrapper.find('[role="dialog"]').exists()).toBe(true)
  })
})
