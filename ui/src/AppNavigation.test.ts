import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import type { ConfigSnapshot } from './config'
import { contractMonitoring, emptyConfigSnapshot, jsonResponse } from './test/contractFixtures'

const bearerToken = 'test-only-navigation-token'

const snapshot = emptyConfigSnapshot()

afterEach(() => {
  window.location.hash = ''
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('application navigation', () => {
  it('opens the statistics route with only the monitoring request', async () => {
    window.location.hash = '#/stats'
    const fetch = vi.fn(() => Promise.resolve(jsonResponse(contractMonitoring())))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('a[href="#/stats"]').attributes('aria-current')).toBe('page')
    expect(wrapper.get('.haproxy-stats').text()).toContain('Statistics')
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(fetch).toHaveBeenCalledWith('/api/v1/monitoring', expect.objectContaining({ cache: 'no-store' }))
    expect(wrapper.find('.monitoring-overview').exists()).toBe(false)
    expect(wrapper.find('.topology-section').exists()).toBe(false)
    wrapper.unmount()
  })

  it('opens the configuration route without starting monitoring requests', async () => {
    window.location.hash = '#/configuration'
    const fetch = vi.fn(() => Promise.resolve(new Response(JSON.stringify(snapshot))))
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)

    expect(wrapper.get('.app-navigation').attributes('aria-label')).toBe('Primary navigation')
    expect(wrapper.get('a[href="#/configuration"]').attributes('aria-current')).toBe('page')
    expect(wrapper.get('.config-workspace').text()).toContain('Configuration workspace')
    expect(fetch).not.toHaveBeenCalled()
    await unlockConfiguration(wrapper)
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(fetch).toHaveBeenCalledWith('/api/v1/config', expect.objectContaining({
      cache: 'no-store',
      headers: { Authorization: `Bearer ${bearerToken}` },
    }))
    expect(wrapper.attributes('aria-busy')).toBe('false')
    expect(wrapper.find('.monitoring-overview').exists()).toBe(false)
    wrapper.unmount()
  })

  it('preserves a dirty draft and validation errors across topology navigation', async () => {
    window.location.hash = '#/configuration'
    const diagnostic = {
      code: 'E_VERSION',
      severity: 'error',
      stage: 'validation',
      message: 'Version must remain 1.',
    }
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(snapshot)
      if (url === '/api/v1/config/validate') {
        const body = JSON.parse(String(init?.body)) as { config: ConfigSnapshot['config'] }
        return jsonResponse({
          candidateRevision: 'candidate',
          normalizedConfig: body.config,
          configFormat: 'uci',
          compositional: false,
          dependencyCount: 0,
          configPreview: "config 'json' 'root'\n",
          diagnostics: [diagnostic],
          restartRequired: false,
          topology: {
            schemaVersion: 1,
            state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1 },
            nodes: [],
            edges: [],
            overlays: [],
          },
        }, 422)
      }
      if (url === '/api/v1/topology') {
        return jsonResponse({
          schemaVersion: 1,
          state: { config: 'active', runtime: 'active', sampledAtUnixMs: 1 },
          nodes: [],
          edges: [],
          overlays: [],
        })
      }
      return jsonResponse({ error: { message: 'not needed in this navigation test' } }, 503)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await unlockConfiguration(wrapper)
    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.diagnostic-list').text()).toContain('E_VERSION')
    await wrapper.get('a[href="#/overview"]').trigger('click')
    await flushPromises()
    expect(wrapper.find('.config-workspace').exists()).toBe(false)
    expect(wrapper.get('.topology-section').text()).toContain('Network topology')
    const unload = new Event('beforeunload', { cancelable: true })
    expect(window.dispatchEvent(unload)).toBe(false)
    await wrapper.get('a[href="#/configuration"]').trigger('click')
    await wrapper.vm.$nextTick()

    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('2')
    expect(wrapper.get('.diagnostic-list').text()).toContain('E_VERSION')
    expect(fetch.mock.calls.filter(([url]) => String(url) === '/api/v1/config')).toHaveLength(1)
    wrapper.unmount()
  })
})

function findButton(wrapper: ReturnType<typeof mount>, text: string) {
  const button = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!button) throw new Error(`Button not found: ${text}`)
  return button
}

async function unlockConfiguration(wrapper: ReturnType<typeof mount>): Promise<void> {
  await wrapper.get('#config-access-token').setValue(bearerToken)
  await wrapper.get('form[data-unlock-form]').trigger('submit')
  await flushPromises()
}
