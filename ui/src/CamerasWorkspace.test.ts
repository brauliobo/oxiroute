import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'
import { camerasCatalog, camerasStats } from './test/camerasFixtures'
import { contractMonitoring, jsonResponse } from './test/contractFixtures'
import { flushPromises, mount } from './test/mount'
import { signIn } from './test/session'

const paths = (fetch: ReturnType<typeof vi.fn>) =>
  fetch.mock.calls.map((call) => String((call as unknown as [RequestInfo | URL])[0]).split('?')[0])

function route(url: string): Response {
  const path = url.split('?')[0]
  if (path === '/api/v1/monitoring') return jsonResponse(contractMonitoring())
  if (path === '/api/v1/rtmp/streams') return jsonResponse(camerasCatalog())
  if (path === '/api/v1/rtmp/stats') return jsonResponse(camerasStats())
  if (path === '/api/v1/rtmp/recordings') return jsonResponse({ recordings: [], next_cursor: null, roots: [] })
  if (path === '/api/v1/rtmp/secrets') return jsonResponse({ secrets: [] })
  return jsonResponse({ error: { code: 'not_found', message: 'not found' } }, 404)
}

beforeEach(() => {
  window.location.hash = '#/cameras'
  signIn()
  vi.spyOn(HTMLMediaElement.prototype, 'canPlayType').mockReturnValue('maybe')
})

afterEach(() => {
  window.location.hash = ''
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe('cameras route', () => {
  it('is in the navigation, titles the document and reads the stream catalog and client list but not the topology', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => Promise.resolve(route(String(input))))
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('a[href="#/cameras"]').attributes('aria-current')).toBe('page')
    expect(document.title).toBe('Cameras and recordings - OxiRoute')
    expect(paths(fetch)).toEqual(expect.arrayContaining(['/api/v1/monitoring', '/api/v1/rtmp/streams', '/api/v1/rtmp/stats']))
    expect(paths(fetch)).not.toContain('/api/v1/topology')
    expect(wrapper.findAll('[data-camera]')).toHaveLength(6)
  })

  it('switches between streams, recordings and access keys with the keyboard-operable tabs', async () => {
    vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL) => Promise.resolve(route(String(input)))))
    const wrapper = mount(App)
    await flushPromises()

    const tabs = wrapper.findAll('[role="tab"]')
    expect(tabs.map((tab) => tab.text())).toEqual(['Streams', 'Recordings', 'Access keys'])
    expect(tabs.map((tab) => tab.attributes('aria-selected'))).toEqual(['true', 'false', 'false'])

    await tabs[1]!.trigger('click')
    await flushPromises()
    expect(wrapper.find('[data-camera]').exists()).toBe(false)
    expect(wrapper.get('[role="tabpanel"]').attributes('aria-labelledby')).toBe(wrapper.findAll('[role="tab"]')[1]!.attributes('id'))

    await wrapper.get('[role="tablist"]').trigger('keydown', { key: 'ArrowRight' })
    await flushPromises()
    expect(wrapper.findAll('[role="tab"]')[2]!.attributes('aria-selected')).toBe('true')
  })

  it('refreshes the stream data right away when the route is opened from another one', async () => {
    window.location.hash = '#/stats'
    const fetch = vi.fn((input: RequestInfo | URL) => Promise.resolve(route(String(input))))
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()
    expect(paths(fetch)).not.toContain('/api/v1/rtmp/streams')

    const changed = new Promise((resolve) => window.addEventListener('hashchange', resolve, { once: true }))
    await wrapper.get('a[href="#/cameras"]').trigger('click')
    await changed
    await flushPromises()
    expect(paths(fetch)).toContain('/api/v1/rtmp/streams')
    expect(wrapper.findAll('[data-camera]')).toHaveLength(6)
  })
})
