import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import CertificatesWorkspace from './CertificatesWorkspace.vue'
import { pendingConfirm, settleConfirm } from './shell/confirm'
import { flushPromises, mount } from './test/mount'
import { jsonResponse } from './test/contractFixtures'
import { managementGeneration, managementTlsInventory } from './test/managementFixtures'
import type { TlsInventory } from './api'

const token = 'certificates-test-token'
const emptyEvents = { events: [], cursor: 0, latestCursor: 0, hasMore: false, oldestCursor: null }
const shutdown = () => new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
  headers: { 'Content-Type': 'text/event-stream' },
})

interface Wire {
  inventory?: TlsInventory
  events?: unknown
  post?: (url: string) => Response
}

function stubApi({ inventory = managementTlsInventory(), events = emptyEvents, post }: Wire = {}) {
  const requests: Array<{ url: string; method: string }> = []
  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    requests.push({ url, method: init?.method ?? 'GET' })
    if (init?.method === 'POST') return Promise.resolve(post!(url))
    if (url === '/api/v1/tls') return Promise.resolve(jsonResponse(inventory))
    if (url === '/api/v1/generations') return Promise.resolve(jsonResponse(managementGeneration()))
    if (url.startsWith('/api/v2/events?')) return Promise.resolve(jsonResponse(events))
    if (url === '/api/v2/events/stream') return Promise.resolve(shutdown())
    throw new Error(`Unexpected request: ${url}`)
  }))
  return requests
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date('2026-07-12T00:00:00Z'))
})

afterEach(() => {
  settleConfirm(false)
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('certificates workspace', () => {
  it('lists the inventory with a count, console timestamps and expiry badges, without an auth panel', async () => {
    stubApi()
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('h2').text()).toBe('Certificates')
    expect(wrapper.text()).toContain('2 certificates')
    expect(wrapper.text()).not.toContain('Management token required')
    expect(wrapper.text()).toContain('2026-08-01 00:00:00 UTC, expires in 20 days')
    expect(wrapper.findAll('.ox-cert')).toHaveLength(2)
    expect(wrapper.text()).toContain('Expires soon')
  })

  it('says "1 certificate" and shows an honest empty state for no jobs', async () => {
    const inventory = managementTlsInventory()
    inventory.certificates = inventory.certificates.slice(0, 1)
    stubApi({ inventory })
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('1 certificate')
    expect(wrapper.text()).not.toContain('1 certificates')
    expect(wrapper.text()).toContain('No certificate job events are retained in the current event ring.')
  })

  it('shows an empty inventory as an empty state', async () => {
    stubApi({ inventory: { certificates: [], watcher: null } })
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('No certificates are configured')
    expect(wrapper.find('.ox-cert').exists()).toBe(false)
  })

  it('lists certificate job events newest first in a keyboard-reachable table', async () => {
    stubApi({
      events: {
        events: [
          { cursor: 4, timestampUnixMs: null, event: 'certificate_renewal', outcome: 'requested', revision: 'active-revision', certificate: 'managed-edge' },
          { cursor: 5, timestampUnixMs: 1_754_000_000_000, event: 'certificate_activated', outcome: 'activated', revision: null, certificate: 'managed-edge' },
          { cursor: 6, timestampUnixMs: null, event: 'generation_prepare', outcome: 'prepared', revision: null },
        ],
        cursor: 6,
        latestCursor: 6,
        hasMore: false,
        oldestCursor: 4,
      },
    })
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    const rows = wrapper.findAll('tbody tr').map((row) => row.text())
    expect(rows).toHaveLength(2)
    expect(rows[0]).toContain('Certificate activated')
    expect(rows[0]).toContain('Activated')
    expect(rows[1]).toContain('No timestamp')
    expect(wrapper.get('.ox-scroll').attributes()).toMatchObject({ tabindex: '0', role: 'region' })
    expect(wrapper.text()).toContain('2 job events')
  })

  it('asks before acting and sends nothing when the dialog is dismissed', async () => {
    const requests = stubApi()
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    await wrapper.findAll('button').find((button) => button.text() === 'Renew now')!.trigger('click')
    await flushPromises()
    expect(pendingConfirm.value).toMatchObject({ title: 'Renew managed-edge?', confirmLabel: 'Renew now', danger: false })
    settleConfirm(false)
    await flushPromises()

    expect(requests.some(({ method }) => method === 'POST')).toBe(false)
  })

  it('marks destructive actions as dangerous and reloads after a conflict', async () => {
    const requests = stubApi({
      post: () => jsonResponse({ error: { code: 'revision_conflict', message: 'Active revision changed.' } }, 409),
    })
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    await wrapper.findAll('button').find((button) => button.text() === 'Revoke certificate')!.trigger('click')
    await flushPromises()
    expect(pendingConfirm.value).toMatchObject({ title: 'Revoke managed-edge at the ACME CA?', danger: true })
    settleConfirm(true)
    await flushPromises()

    expect(wrapper.get('[role="alert"]').text()).toContain('Active revision changed.')
    expect(requests.filter(({ url }) => url === '/api/v1/tls')).toHaveLength(2)
  })

  it('relocks on an unauthorized response', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({ error: { code: 'unauthorized', message: 'no' } }, 401))))
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.emitted('unauthorized')).toBeTruthy()
  })
})
