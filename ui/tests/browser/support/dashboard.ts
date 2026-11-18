import type { Page } from '@playwright/test'

import { contractCatalog, contractMonitoring, contractRtmpStats, contractTopology } from '../../../src/test/contractFixtures'
import { installApiMock, json, requestPath } from '../support'
import type { ApiMockResponse } from '../support'

export const THEMES = ['light', 'dark'] as const
export type DemoTheme = (typeof THEMES)[number]

const SAMPLED_AT = 1_750_000_000_000

function demoMonitoring() {
  const monitoring = contractMonitoring()
  monitoring.sampledAtUnixMs = SAMPLED_AT
  const [primary, secondary] = monitoring.upstreamPools[0]!.endpoints as [
    (typeof monitoring.upstreamPools)[number]['endpoints'][number],
    (typeof monitoring.upstreamPools)[number]['endpoints'][number],
  ]
  for (const endpoint of [primary, secondary]) {
    endpoint.lastCheckedAtUnixMs = SAMPLED_AT - 4_000
    endpoint.lastTransitionAtUnixMs = SAMPLED_AT - 90_000
  }
  const ejected = { ...structuredClone(primary), name: 'ejected-origin', address: '192.0.2.31:3000', passiveEjected: true }
  const unchecked = {
    ...structuredClone(primary), name: 'unchecked-origin', address: '192.0.2.32:3000', state: 'unchecked' as const,
    lastCheckedAtUnixMs: null, lastTransitionAtUnixMs: null, checksEnabled: false, checksRunning: false,
  }
  const base = monitoring.upstreamPools[0]!
  monitoring.upstreamPools.push(
    { ...structuredClone(base), name: 'static-assets', availableEndpoints: 1, totalEndpoints: 2, endpoints: [ejected, unchecked] },
    { ...structuredClone(base), name: 'reserve', availableEndpoints: 0, totalEndpoints: 0, endpoints: [] },
  )
  return monitoring
}

function demoRtmpStats() {
  const stats = contractRtmpStats()
  stats.clients = [{
    id: 'client-session-0001',
    service: 'edge',
    application: 'live',
    stream: 'camera',
    role: 'subscriber',
    peerIp: '192.0.2.77',
    connectedAtUnixMs: SAMPLED_AT,
    revision: '4',
  }]
  return stats
}

function demoTopology() {
  const topology = contractTopology()
  topology.nodes[0]!.name = 'edge-public-https-listener-with-a-deliberately-long-name'
  return topology
}

export function demoDashboardResponse(path: string): ApiMockResponse | undefined {
  if (path === '/api/v1/monitoring') return json(demoMonitoring())
  if (path === '/api/v1/rtmp/streams') return json(contractCatalog())
  if (path === '/api/v1/rtmp/stats') return json(demoRtmpStats())
  if (path === '/api/v1/topology') return json(demoTopology())
  return undefined
}

export async function installDemoDashboard(page: Page, theme: DemoTheme): Promise<void> {
  await page.addInitScript((value) => localStorage.setItem('oxiroute.theme', value), theme)
  await installApiMock(page, (request) => demoDashboardResponse(requestPath(request)))
}
