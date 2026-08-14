import { flushPromises, mount } from '@vue/test-utils'
import type { Component } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'

import CertificatesWorkspace from './CertificatesWorkspace.vue'
import EventsWorkspace from './EventsWorkspace.vue'
import OperationsWorkspace from './OperationsWorkspace.vue'
import ProvenanceWorkspace from './ProvenanceWorkspace.vue'
import { jsonResponse } from './test/contractFixtures'
import { importReportResponse } from './test/importFixtures'
import {
  managementGeneration,
  managementListeners,
  managementPools,
  managementServers,
  managementStatus,
  managementTlsInventory,
} from './test/managementFixtures'

const tokenA = 'token-a'
const tokenB = 'token-b'

interface PendingRequest {
  url: string
  token: string
  signal: AbortSignal
  resolve: (response: Response) => void
}

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

describe('workspace request ownership', () => {
  it('replaces event history requests when the token changes', async () => {
    await expectReplacement(EventsWorkspace, 1, eventResponse, 'stale-a', 'current-b')
  })

  it('replaces certificate inventory requests when the token changes', async () => {
    await expectReplacement(CertificatesWorkspace, 3, certificateResponse, 'stale-a-certificate', 'current-b-certificate')
  })

  it('replaces operations inventory requests when the token changes', async () => {
    await expectReplacement(OperationsWorkspace, 5, operationsResponse, 'stale-a', 'current-b')
  })

  it('replaces provenance inventory requests when the token changes', async () => {
    await expectProvenanceReplacement(false)
    await expectProvenanceReplacement(true)
  })
})

async function expectReplacement(
  component: Component,
  requestCount: number,
  responseFor: (url: string, marker: string) => Response,
  staleMarker: string,
  currentMarker: string,
): Promise<void> {
  await expectReplacementRound(component, requestCount, responseFor, staleMarker, currentMarker, false)
  await expectReplacementRound(component, requestCount, responseFor, staleMarker, currentMarker, true)
}

async function expectReplacementRound(
  component: Component,
  requestCount: number,
  responseFor: (url: string, marker: string) => Response,
  staleMarker: string,
  currentMarker: string,
  staleUnauthorized: boolean,
): Promise<void> {
  const requests: PendingRequest[] = []
  vi.stubGlobal('fetch', controlledFetch(requests))
  const wrapper = mount(component, { props: { token: tokenA } })
  await wrapper.vm.$nextTick()

  const requestsA = requestsFor(requests, tokenA)
  expect(requestsA).toHaveLength(requestCount)

  await wrapper.setProps({ token: tokenB })
  const requestsB = requestsFor(requests, tokenB)
  expect(requestsB).toHaveLength(requestCount)
  expect(requestsA.every(({ signal }) => signal.aborted)).toBe(true)

  for (const request of requestsA) {
    request.resolve(staleUnauthorized
      ? jsonResponse({ error: { code: 'unauthorized', message: 'stale token rejected' } }, 401)
      : responseFor(request.url, staleMarker))
  }
  await flushPromises()

  expect(wrapper.attributes('aria-busy')).toBe('true')
  expect(wrapper.text()).not.toContain(staleMarker)
  expect(wrapper.emitted('unauthorized')).toBeUndefined()

  for (const request of requestsB) request.resolve(responseFor(request.url, currentMarker))
  await flushPromises()

  expect(wrapper.attributes('aria-busy')).not.toBe('true')
  expect(wrapper.text()).toContain(currentMarker)
  expect(wrapper.emitted('unauthorized')).toBeUndefined()
  wrapper.unmount()
}

async function expectProvenanceReplacement(staleUnauthorized: boolean): Promise<void> {
  const requests: PendingRequest[] = []
  vi.stubGlobal('fetch', controlledFetch(requests))
  const wrapper = mount(ProvenanceWorkspace, { props: { token: tokenA } })
  await wrapper.vm.$nextTick()

  const inventoryA = requestsFor(requests, tokenA)
  expect(inventoryA).toHaveLength(1)

  await wrapper.setProps({ token: tokenB })
  const inventoryB = requestsFor(requests, tokenB)
  expect(inventoryB).toHaveLength(1)
  expect(inventoryA[0]!.signal.aborted).toBe(true)

  inventoryA[0]!.resolve(staleUnauthorized
    ? jsonResponse({ error: { code: 'unauthorized', message: 'stale token rejected' } }, 401)
    : provenanceResponse('stale-a-profile'))
  await flushPromises()

  expect(wrapper.attributes('aria-busy')).toBe('true')
  expect(wrapper.text()).not.toContain('stale-a-profile')
  expect(wrapper.emitted('unauthorized')).toBeUndefined()

  inventoryB[0]!.resolve(provenanceResponse('current-b-profile'))
  await flushPromises()
  const detailB = requestsFor(requests, tokenB).filter(({ url }) => url.endsWith('/0'))
  expect(detailB).toHaveLength(1)
  detailB[0]!.resolve(provenanceResponse('current-b-profile'))
  await flushPromises()

  expect(wrapper.attributes('aria-busy')).toBe('false')
  expect(wrapper.text()).toContain('current-b-profile')
  expect(wrapper.text()).not.toContain('stale-a-profile')
  expect(wrapper.emitted('unauthorized')).toBeUndefined()
  wrapper.unmount()
}

function controlledFetch(requests: PendingRequest[]) {
  return vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v2/events/stream') {
      return Promise.resolve(new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
        headers: { 'Content-Type': 'text/event-stream' },
      }))
    }
    return new Promise<Response>((resolve) => {
      requests.push({
        url,
        token: authorizationToken(init),
        signal: init?.signal as AbortSignal,
        resolve,
      })
    })
  })
}

function requestsFor(requests: PendingRequest[], token: string): PendingRequest[] {
  return requests.filter((request) => request.token === token)
}

function authorizationToken(init?: RequestInit): string {
  const authorization = (init?.headers as Record<string, string> | undefined)?.Authorization ?? ''
  return authorization.replace('Bearer ', '')
}

function eventResponse(_url: string, marker: string): Response {
  return jsonResponse({
    events: [{
      cursor: marker.startsWith('stale') ? 1 : 2,
      timestampUnixMs: null,
      event: 'generation_activate',
      outcome: 'activated',
      revision: marker,
    }],
    cursor: marker.startsWith('stale') ? 1 : 2,
    latestCursor: marker.startsWith('stale') ? 1 : 2,
    hasMore: false,
    oldestCursor: 1,
  })
}

function certificateResponse(url: string, marker: string): Response {
  if (url === '/api/v1/tls') {
    const inventory = managementTlsInventory()
    inventory.certificates[0]!.name = marker
    return jsonResponse(inventory)
  }
  if (url === '/api/v1/generations') return jsonResponse(managementGeneration())
  if (url.startsWith('/api/v2/events?')) {
    return jsonResponse({ events: [], cursor: 0, latestCursor: 0, hasMore: false, oldestCursor: null })
  }
  throw new Error(`Unexpected certificate request: ${url}`)
}

function operationsResponse(url: string, marker: string): Response {
  if (url === '/api/v1/status') {
    const status = managementStatus()
    status.activeRevision = marker
    return jsonResponse(status)
  }
  if (url === '/api/v1/generations') {
    const generation = managementGeneration()
    generation.generation.activeRevision = marker
    return jsonResponse(generation)
  }
  if (url === '/api/v1/listeners') return jsonResponse(managementListeners())
  if (url === '/api/v1/pools') return jsonResponse(managementPools())
  if (url === '/api/v1/servers') return jsonResponse(managementServers())
  throw new Error(`Unexpected operations request: ${url}`)
}

function provenanceResponse(marker: string): Response {
  const report = importReportResponse()
  if (!report.report) throw new Error('Expected a report fixture')
  report.reports[0]!.capabilityProfile.id = marker
  report.report.source.capabilityProfile.id = marker
  return jsonResponse(report)
}
