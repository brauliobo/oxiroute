import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import CertificatesWorkspace from './CertificatesWorkspace.vue'
import EventsWorkspace from './EventsWorkspace.vue'
import OperationsWorkspace from './OperationsWorkspace.vue'
import { fetchTlsInventory } from './api'
import { jsonResponse } from './test/contractFixtures'
import {
  managementGeneration,
  managementListeners,
  managementPools,
  managementServers,
  managementStatus,
  managementTlsInventory,
} from './test/managementFixtures'
import { importReportResponse } from './test/importFixtures'

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
    await wrapper.get('.management-auth').trigger('submit')
    await flushPromises()

    expect(wrapper.get('a[href="#/provenance"]').attributes('aria-current')).toBe('page')
    expect(wrapper.get('.provenance-workspace').text()).toContain('apache import')
    expect(fetch.mock.calls.some(([url]) => String(url).includes('/api/v1/import-reports'))).toBe(true)
    wrapper.unmount()
  })

  it('shows an authentication error without creating operational placeholder state', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'unauthorized', message: 'invalid bearer token' },
    }, 401))))

    const wrapper = mount(OperationsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.operation-error').text()).toContain('invalid bearer token')
    expect(wrapper.find('.status-section').exists()).toBe(false)
    expect(wrapper.find('.inventory-grid').exists()).toBe(false)
    wrapper.unmount()
  })

  it('requires confirmation and an active revision before destructive generation actions', async () => {
    const fetch = operationsFetch()
    const confirm = vi.fn(() => false)
    vi.stubGlobal('fetch', fetch)
    vi.stubGlobal('confirm', confirm)
    const wrapper = mount(OperationsWorkspace, { props: { token } })
    await flushPromises()

    await wrapper.get('[data-generation-action="rollback"]').trigger('click')
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining('active revision'))
    expect(fetch.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(0)

    confirm.mockReturnValue(true)
    await wrapper.get('[data-generation-action="reload"]').trigger('click')
    await flushPromises()
    const reloadCall = fetch.mock.calls.find(([url, init]) => String(url) === '/api/v1/generations/reload' && init?.method === 'POST')
    expect(reloadCall?.[1]?.body).toBe(JSON.stringify({ expectedActiveRevision: 'active-revision' }))
    wrapper.unmount()
  })

  it('resynchronizes the bounded event view after the server reports cursor loss', async () => {
    let eventPages = 0
    let streams = 0
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url.startsWith('/api/v1/events?')) {
        eventPages += 1
        return Promise.resolve(jsonResponse(eventPages === 1 ? {
          events: [{ cursor: 1, timestampUnixMs: null, event: 'generation_prepare', outcome: 'prepared', revision: null }],
          cursor: 1,
          hasMore: false,
          oldestCursor: 1,
        } : {
          events: [{ cursor: 9, timestampUnixMs: null, event: 'generation_activate', outcome: 'activated', revision: 'active-revision' }],
          cursor: 9,
          hasMore: false,
          oldestCursor: 9,
        }))
      }
      if (url === '/api/v1/events/stream') {
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

    expect(wrapper.get('.resync-banner').text()).toContain('resynchronized')
    expect(wrapper.text()).toContain('generation activate')
    expect(eventPages).toBe(2)
    expect(streams).toBe(2)
    wrapper.unmount()
  })

  it('renders certificate jobs without exposing private or account material', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/tls') return Promise.resolve(jsonResponse(managementTlsInventory()))
      if (url === '/api/v1/generations') return Promise.resolve(jsonResponse(managementGeneration()))
      if (url.startsWith('/api/v1/events?')) return Promise.resolve(jsonResponse({ events: [], cursor: 0, hasMore: false, oldestCursor: null }))
      if (url === '/api/v1/events/stream') return Promise.resolve(new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', { headers: { 'Content-Type': 'text/event-stream' } }))
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.certificates-workspace').text()).toContain('managed-edge')
    expect(wrapper.text()).not.toContain('privateKey')
    expect(wrapper.text()).not.toContain('accountUrl')
    expect(wrapper.text()).not.toContain('orderUrl')
    wrapper.unmount()
  })

  it('keeps operational controls in a mobile-ready responsive grid', async () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    const fetch = operationsFetch()
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(OperationsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.find('.inventory-grid').exists()).toBe(true)
    expect(wrapper.find('.server-controls').exists()).toBe(true)
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
      if (url.startsWith('/api/v1/events?')) {
        return Promise.resolve(jsonResponse({ events: [], cursor: 0, hasMore: false, oldestCursor: null }))
      }
      if (url === '/api/v1/events/stream') {
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
    const confirm = vi.fn(() => true)
    vi.stubGlobal('fetch', fetch)
    vi.stubGlobal('confirm', confirm)

    const wrapper = mount(CertificatesWorkspace, { props: { token } })
    await flushPromises()
    const revokeButton = wrapper.findAll('button').find((button) => button.text() === 'Revoke certificate')
    if (!revokeButton) throw new Error('revoke action was not rendered')

    await revokeButton.trigger('click')
    await flushPromises()

    const request = requests.find(({ url, init }) => url === '/api/v1/tls/revoke' && init?.method === 'POST')
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining('Active revision'))
    expect(request).toBeDefined()
    expect(JSON.parse(String(request?.init?.body))).toEqual({
      expectedActiveRevision: 'active-revision',
      certificate: 'managed-edge',
    })
    wrapper.unmount()
  })
})

function operationsFetch() {
  return vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/status') return Promise.resolve(jsonResponse(managementStatus()))
    if (url === '/api/v1/generations') return Promise.resolve(jsonResponse(managementGeneration()))
    if (url === '/api/v1/listeners') return Promise.resolve(jsonResponse(managementListeners()))
    if (url === '/api/v1/pools') return Promise.resolve(jsonResponse(managementPools()))
    if (url === '/api/v1/servers') return Promise.resolve(jsonResponse(managementServers()))
    if (init?.method === 'POST' || init?.method === 'PUT') return Promise.resolve(jsonResponse({ outcome: 'applied', changed: 1 }))
    throw new Error(`Unexpected request: ${url}`)
  })
}
