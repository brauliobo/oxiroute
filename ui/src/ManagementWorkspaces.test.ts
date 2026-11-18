import { flushPromises, mount } from './test/mount'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import { signIn } from './test/session'
import CertificatesWorkspace from './CertificatesWorkspace.vue'
import EventsWorkspace from './EventsWorkspace.vue'
import { fetchTlsInventory } from './api'
import { jsonResponse } from './test/contractFixtures'
import { managementGeneration, managementStatus, managementTlsInventory } from './test/managementFixtures'
import { importReportResponse } from './test/importFixtures'
import { pendingConfirm, settleConfirm } from './shell/confirm'

const token = 'management-ui-test-token'

afterEach(() => {
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 })
  window.location.hash = ''
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

describe('local management workspaces', () => {
  it('navigates to provenance and renders the native import report contract', async () => {
    window.location.hash = '#/provenance'
    const report = importReportResponse()
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse({
        ...managementStatus(),
        sampledAtUnixMs: Date.now(),
        uptimeMs: 1,
        process: { activeConnections: 0, administrativeState: 'ready', cpuPercent: null, maxConnections: null, rejectedConnections: '0', retryAttempts: '0', residentMemoryBytes: 0, virtualMemoryBytes: 0, threadCount: 1, openFileDescriptors: 1 },
        host: { loadAverage1m: 0, loadAverage5m: 0, loadAverage15m: 0, totalMemoryBytes: 1, availableMemoryBytes: 1 },
        traffic: { acceptedConnections: '0', rejectedConnections: '0', activeConnections: 0, bytesReceived: '0', bytesSent: '0' },
        listeners: [], upstreamPools: [], certbotCertificates: [], certbotWatcher: null, acmeManagedCertificates: [],
        rtmp: { activeStreams: 0, publishers: 0, subscribers: 0, mediaPayloadBytesReceived: '0', recordingSupported: false, manualRecording: false, recorderBytesWritten: '0', recorderSegmentsStarted: '0', recorderSegmentsCompleted: '0', recorderDiscontinuities: '0', relayConnectionAttempts: '0', relayConnections: '0', relayReconnects: '0', relayEventsSent: '0', relayEventsDropped: '0', relayPayloadBytesSent: '0', relays: [], recorders: [] },
      }))
      if (url === '/api/v1/import-reports' || url === '/api/v1/import-reports/0') return Promise.resolve(jsonResponse(report))
      throw new Error(`Unexpected request: ${url} ${init?.method ?? 'GET'}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()
    await wrapper.get('#management-access-token').setValue(token)
    await wrapper.get('form.ox-login-form').trigger('submit')
    await flushPromises()

    expect(wrapper.get('a[href="#/provenance"]').attributes('aria-current')).toBe('page')
    expect(wrapper.get('main').text()).toContain('apache import')
    expect(fetch.mock.calls.some(([url]) => String(url).includes('/api/v1/import-reports'))).toBe(true)
    wrapper.unmount()
  })

  it('resynchronizes the bounded event view after the server reports cursor loss', async () => {
    let eventPages = 0
    let streams = 0
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url.startsWith('/api/v2/events?')) {
        eventPages += 1
        return Promise.resolve(jsonResponse(eventPages === 1 ? {
          events: [{ cursor: 1, timestampUnixMs: null, event: 'generation_prepare', outcome: 'prepared', revision: null }],
          cursor: 1,
          latestCursor: 1,
          hasMore: false,
          oldestCursor: 1,
        } : {
          events: [{ cursor: 9, timestampUnixMs: null, event: 'generation_activate', outcome: 'activated', revision: 'active-revision' }],
          cursor: 9,
          latestCursor: 9,
          hasMore: false,
          oldestCursor: 9,
        }))
      }
      if (url === '/api/v2/events/stream') {
        streams += 1
        return Promise.resolve(new Response(streams === 1
          ? 'event: resync_required\ndata: {"cursor":1,"oldestCursor":9,"latestCursor":9}\n\n'
          : 'event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
          headers: { 'Content-Type': 'text/event-stream; charset=utf-8' },
        }))
      }
      throw new Error(`Unexpected request: ${url} ${init?.method ?? 'GET'}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('resynchronized')
    expect(wrapper.text()).toContain('Generation activate')
    expect(eventPages).toBe(2)
    expect(streams).toBe(2)
    wrapper.unmount()
  })

  it('renders bounded structured endpoint outcomes without object coercion', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url.startsWith('/api/v2/events?')) {
        return Promise.resolve(jsonResponse({
          events: [{
            cursor: 12,
            timestampUnixMs: null,
            event: 'upstream_endpoint_ejection',
            outcome: {
              type: 'ejected',
              pool: 'backend',
              server: 'primary',
              reason: 'connect_failed',
              failureCount: 3,
              ejectionCount: 2,
              ejectedAtUnixMs: 100,
              ejectionUntilUnixMs: 200,
            },
            revision: null,
          }],
          cursor: 12,
          latestCursor: 12,
          hasMore: false,
          oldestCursor: 12,
        }))
      }
      if (url === '/api/v2/events/stream') {
        return Promise.resolve(new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
          headers: { 'Content-Type': 'text/event-stream' },
        }))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('tbody .ox-badge').text()).toBe('Ejected')
    expect(wrapper.text()).toContain('backend / primary / connect failed')
    expect(wrapper.text()).not.toContain('[object Object]')
    wrapper.unmount()
  })

  it('renders certificate jobs without exposing private or account material', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/tls') return Promise.resolve(jsonResponse(managementTlsInventory()))
      if (url === '/api/v1/generations') return Promise.resolve(jsonResponse(managementGeneration()))
      if (url.startsWith('/api/v2/events?')) return Promise.resolve(jsonResponse({ events: [], cursor: 0, latestCursor: 0, hasMore: false, oldestCursor: null }))
      if (url === '/api/v2/events/stream') return Promise.resolve(new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', { headers: { 'Content-Type': 'text/event-stream' } }))
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('managed-edge')
    expect(wrapper.text()).not.toContain('privateKey')
    expect(wrapper.text()).not.toContain('accountUrl')
    expect(wrapper.text()).not.toContain('orderUrl')
    wrapper.unmount()
  })

  it('redacts unsupported TLS inventory fields at the API boundary', async () => {
    const fetch = vi.fn(() => Promise.resolve(jsonResponse({
      certificates: [{
        name: 'managed-edge',
        dnsNames: ['edge.example.test'],
        source: 'acme_managed',
        developmentOnly: false,
        status: {
          certificate: 'managed-edge',
          directoryUrl: 'https://acme.example.test/directory',
          challenge: 'dns01',
          dnsProvider: 'fake',
          keyType: 'ecdsa_p256',
          allowedDnsSuffixes: ['example.test'],
          diskRevision: 'disk',
          activeRevision: 'active',
          notBeforeUnixSeconds: null,
          notAfterUnixSeconds: null,
          nextActionUnixSeconds: null,
           notAfter: '2026-08-01T00:00:00Z',
           jobStatus: null,
           jobId: null,
           paused: false,
           retainedRevisions: 3,
           retentionDays: 30,
           retryAttempt: 0,
          lastSuccessUnixSeconds: null,
          lastOutcome: null,
          lastErrorCode: null,
          privateKey: 'private-key-material',
          accountUrl: 'https://acme.example.test/acct/secret',
          orderUrl: 'https://acme.example.test/order/secret',
          authorizationToken: 'challenge-secret',
        },
      }],
      watcher: null,
    })))
    vi.stubGlobal('fetch', fetch)

    const inventory = await fetchTlsInventory(token)
    expect(JSON.stringify(inventory)).not.toContain('private-key-material')
    expect(JSON.stringify(inventory)).not.toContain('accountUrl')
    expect(JSON.stringify(inventory)).not.toContain('orderUrl')
    expect(JSON.stringify(inventory)).not.toContain('challenge-secret')
  })

  it('sends the active revision for confirmed managed ACME actions', async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = []
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      requests.push({ url, init })
      if (url === '/api/v1/tls') return Promise.resolve(jsonResponse(managementTlsInventory()))
      if (url === '/api/v1/generations') return Promise.resolve(jsonResponse(managementGeneration()))
      if (url.startsWith('/api/v2/events?')) {
        return Promise.resolve(jsonResponse({ events: [], cursor: 0, latestCursor: 0, hasMore: false, oldestCursor: null }))
      }
      if (url === '/api/v2/events/stream') {
        return Promise.resolve(new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
          headers: { 'Content-Type': 'text/event-stream' },
        }))
      }
      if (init?.method === 'POST') {
        return Promise.resolve(jsonResponse({
          certificate: 'managed-edge',
          outcome: 'revoked',
          jobId: 'revoke-job-1',
        }))
      }
      throw new Error(`Unexpected request: ${url} ${init?.method ?? 'GET'}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()
    const revokeButton = wrapper.findAll('button').find((button) => button.text() === 'Revoke certificate')
    if (!revokeButton) throw new Error('revoke action was not rendered')

    await revokeButton.trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.message).toContain('Active revision')
    settleConfirm(true)
    await flushPromises()

    const request = requests.find(({ url, init }) => url === '/api/v1/tls/revoke' && init?.method === 'POST')
    expect(request).toBeDefined()
    expect(JSON.parse(String(request?.init?.body))).toEqual({
      expectedActiveRevision: 'active-revision',
      certificate: 'managed-edge',
    })
    wrapper.unmount()
  })
})
