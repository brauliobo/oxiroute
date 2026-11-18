import { afterEach, describe, expect, it, vi } from 'vitest'
import { secretFingerprints } from '../../test/camerasFixtures'
import { jsonResponse } from '../../test/contractFixtures'
import { flushPromises, mount } from '../../test/mount'
import KeysPanel from './KeysPanel.vue'

const token = 'keys-test-token'
const SECRETS = '/api/v1/rtmp/secrets'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

function stub(reply: () => Response = () => jsonResponse({ secrets: secretFingerprints() })) {
  const fetch = vi.fn(() => Promise.resolve(reply()))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

async function open(reply?: () => Response) {
  const fetch = stub(reply)
  const wrapper = mount(KeysPanel, { props: { token } })
  await flushPromises()
  return { wrapper, fetch }
}

describe('access key fingerprints', () => {
  it('asks the server once with the token', async () => {
    const { fetch } = await open()
    expect(fetch).toHaveBeenCalledTimes(1)
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe(SECRETS)
    expect(new Headers(init.headers).get('Authorization')).toBe(`Bearer ${token}`)
  })

  it('explains that only fingerprints are shown and what they are made of', async () => {
    const { wrapper } = await open()
    const text = wrapper.text()

    expect(text).toContain('never shows the keys themselves, only a fingerprint')
    expect(text).toContain('"fp-" followed by the first 8 hex digits of the key\'s SHA-256')
    expect(text).toContain('the same value the callbacks receive in the query field')
    expect(text).toContain('use long random keys')
  })

  it('lists service, application, role and fingerprint of every key', async () => {
    const { wrapper } = await open()
    const rows = wrapper.findAll('tbody tr').map((row) => [row.get('th').text(), ...row.findAll('td').map((cell) => cell.text())])

    expect(wrapper.findAll('thead th').map((head) => head.text())).toEqual(['Service', 'Application', 'Role', 'Fingerprint'])
    expect(rows).toEqual([
      ['edge', 'archive', 'Publish key', 'fp-0a1b2c3d'],
      ['edge', 'live', 'Play key', 'fp-5e6f7a8b'],
      ['edge', 'live', 'Publish key', 'fp-1a2b3c4d'],
    ])
    expect(wrapper.get('[data-key-entry="edge/live/play"] code').classes()).toContain('ox-mono')
  })

  it('renders nothing key-like beyond the fingerprints', async () => {
    const { wrapper } = await open()
    const words = wrapper.text().match(/\b[0-9a-f]{9,}\b/g) ?? []
    expect(words).toEqual([])
    expect(wrapper.findAll('[data-fingerprint]').every((cell) => /^fp-[0-9a-f]{8}$/.test(cell.text()))).toBe(true)
    expect(wrapper.text()).not.toContain(token)
  })

  it('says when no application requires a key', async () => {
    const { wrapper } = await open(() => jsonResponse({ secrets: [] }))

    expect(wrapper.get('.ox-empty').text()).toContain('No application requires a publish or play key')
    expect(wrapper.find('table').exists()).toBe(false)
  })

  it('shows an alert with Retry, and loads again on Retry', async () => {
    let failing = true
    const { wrapper, fetch } = await open(() => (
      failing ? jsonResponse({ error: { code: 'internal', message: 'key inventory unavailable' } }, 500) : jsonResponse({ secrets: secretFingerprints() })
    ))
    expect(wrapper.get('[data-keys-error]').text()).toContain('Keys unavailable.')
    expect(wrapper.get('[data-keys-error]').text()).toContain('key inventory unavailable')
    expect(wrapper.find('table').exists()).toBe(false)

    failing = false
    await wrapper.get('[data-action="retry-keys"]').trigger('click')
    await flushPromises()
    expect(fetch).toHaveBeenCalledTimes(2)
    expect(wrapper.find('[data-keys-error]').exists()).toBe(false)
    expect(wrapper.findAll('tbody tr')).toHaveLength(3)
  })

  it('keeps the last list when a refresh fails', async () => {
    let failing = false
    const { wrapper } = await open(() => (
      failing ? jsonResponse({ error: { code: 'internal', message: 'down' } }, 500) : jsonResponse({ secrets: secretFingerprints() })
    ))
    failing = true
    await wrapper.get('[data-action="refresh-keys"]').trigger('click')
    await flushPromises()

    expect(wrapper.get('[data-keys-error]').text()).toContain('down')
    expect(wrapper.findAll('tbody tr')).toHaveLength(3)
  })

  it('emits unauthorized when the token is rejected', async () => {
    const { wrapper } = await open(() => jsonResponse({ error: { code: 'unauthorized', message: 'invalid bearer token' } }, 401))
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
    expect(wrapper.get('[data-keys-error]').text()).toContain('invalid bearer token')
  })

  it('requests nothing without a token', async () => {
    const fetch = stub()
    mount(KeysPanel, { props: { token: '' } })
    await flushPromises()
    expect(fetch).not.toHaveBeenCalled()
  })
})
