import { flushPromises, mount } from './test/mount'
import { nextTick } from 'vue'
import { describe, expect, it, vi } from 'vitest'

import ConfigurationWorkspace from './ConfigurationWorkspace.vue'
import type { CanonicalConfig } from './config'
import { jsonResponse } from './test/contractFixtures'
import {
  activeRevision,
  bearerToken,
  configSnapshot,
  findButton,
  findButtonIn,
  installConfigFetch,
  mountUnlocked,
} from './test/configurationWorkspace'

describe('ConfigurationWorkspace', () => {
  it('sends the console session token and never stores it', async () => {
    const fetch = installConfigFetch()
    const storageWrite = vi.spyOn(Storage.prototype, 'setItem')
    const wrapper = mount(ConfigurationWorkspace, { props: { token: bearerToken } })
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/config',
      expect.objectContaining({
        headers: expect.objectContaining({ Authorization: `Bearer ${bearerToken}` }),
      }),
    )
    expect(wrapper.find('input[type="password"]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain(bearerToken)
    expect(window.location.href).not.toContain(bearerToken)
    expect(storageWrite).not.toHaveBeenCalled()
  })

  it('round-trips redacted RTMP token secrets through an unrelated edit and save', async () => {
    const snapshot = configSnapshot()
    snapshot.config.rtmp_services[0]!.applications[0]!.publish.token = {
      source: 'stream_query',
      parameter: 'token',
      secret: '<redacted>',
    }
    snapshot.config.rtmp_services[0]!.applications[0]!.play.token = {
      source: 'stream_query',
      parameter: 'viewer',
      secret: '<redacted>',
    }
    const saved: { config: CanonicalConfig | null } = { config: null }
    const fetch = installConfigFetch((config) => {
      saved.config = config
      return jsonResponse({
        diskRevision: 'saved-disk-revision',
        candidateRevision: 'saved-candidate-revision',
        activeRevision,
        outcome: 'saved_pending_activation',
        activationState: 'pending',
        restartRequired: false,
        restartReasons: [],
        diagnostics: [],
      })
    }, 200, snapshot)
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const validated = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(validated.version).toBe(2)
    expect(validated.rtmp_services[0]?.applications[0]?.publish.token?.secret).toBe('<redacted>')
    expect(validated.rtmp_services[0]?.applications[0]?.play.token?.secret).toBe('<redacted>')

    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(saved.config?.version).toBe(2)
    expect(saved.config?.rtmp_services[0]?.applications[0]?.publish.token?.secret).toBe('<redacted>')
    expect(saved.config?.rtmp_services[0]?.applications[0]?.play.token?.secret).toBe('<redacted>')
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
      value: 'ollama.corp-b.test',
    }
    if (route.action.type !== 'proxy') throw new Error('imported route must proxy')
    route.action.policy.retry = {
      ...route.action.policy.retry,
      max_retries: 3,
      target: 'same_server',
      delay_ms: 1_000,
      final_redispatch: true,
      jitter_percent: 0,
      idempotency_header: null,
      budget: null,
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
        restartReasons: [],
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
      value: 'ollama.corp-b.test',
    })
    expect(saved?.http_services[0]?.routes[0]?.action).toMatchObject({
      policy: {
        retry: {
          max_retries: 3,
          target: 'same_server',
          delay_ms: 1_000,
          final_redispatch: true,
          jitter_percent: 0,
          idempotency_header: null,
          budget: null,
        },
        timeouts: { connect_ms: null, header_ms: null, idle_ms: null, total_ms: null },
        rescue: null,
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

    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
    expect(wrapper.text()).not.toContain(bearerToken)

    await wrapper.setProps({ token: '' })
    await wrapper.setProps({ token: refreshedToken })
    await flushPromises()
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

    expect(wrapper.get('.cfg-capability-panel').text()).toContain('Configuration capability unavailable')
    expect(wrapper.get('.cfg-capability-panel').text()).toContain('No placeholder configuration')
    expect(wrapper.find('.cfg-editor-form').exists()).toBe(false)
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

    expect(wrapper.find('.cfg-capability-panel:not(.canonical-unavailable-panel)').exists()).toBe(false)
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('Disk revisionUnknown')
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('E_CONFIG_READ')
    expect(wrapper.get('.canonical-unavailable-panel').text()).toContain('revision is uncertain')
    expect(wrapper.find('.cfg-editor-form').exists()).toBe(false)
  })

  it('does not call an unrelated 503 a missing configuration route', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'service_unavailable', message: 'temporary gateway failure' },
    }, 503))))

    const wrapper = await mountUnlocked()

    expect(wrapper.find('.cfg-capability-panel:not(.load-error-panel)').exists()).toBe(false)
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

  it('exposes disk/active revisions without a false alarm, both certificate sources, and plural SNI profiles', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    expect(wrapper.get('.revision-board').text()).toContain('Disk revision')
    expect(wrapper.get('.revision-board').text()).toContain('Active revision')
    expect(wrapper.find('.revision-banner').exists()).toBe(false)
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
    expect(wrapper.get('.cfg-form-heading').text()).toContain('internal-sni')

    for (const control of wrapper.findAll('input, select')) {
      const element = control.element as HTMLInputElement | HTMLSelectElement
      const hasWrappingLabel = element.closest('label') !== null
      const hasExplicitLabel = Boolean(element.id && document.querySelector(`label[for="${element.id}"]`))
      expect(hasWrappingLabel || hasExplicitLabel || element.hasAttribute('aria-label')).toBe(true)
    }
    wrapper.unmount()
  })
})
