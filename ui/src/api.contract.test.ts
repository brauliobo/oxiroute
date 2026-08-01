import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

import {
  fetchConfig,
  fetchMonitoring,
  fetchTopology,
  validateConfig,
} from './api'
import {
  contractConfigSnapshot,
  contractMonitoring,
  contractTopology,
  emptyConfigSnapshot,
} from './test/contractFixtures'

const token = 'contract-test-token'
const responseOverrides = new Map<string, unknown>()
const server = createServer((request, response) => route(request, response))
let origin = ''
let nativeFetch: typeof fetch

beforeAll(async () => {
  nativeFetch = globalThis.fetch
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address() as AddressInfo
  origin = `http://127.0.0.1:${address.port}`
  vi.stubGlobal('fetch', (input: RequestInfo | URL, init?: RequestInit) =>
    nativeFetch(new URL(String(input), origin), init),
  )
})

afterAll(async () => {
  vi.unstubAllGlobals()
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()))
})

describe('API contracts over HTTP', () => {
  it('parses every tagged canonical HTTP route action over a real HTTP response', async () => {
    const snapshot = await fetchConfig(token)

    expect(snapshot).toEqual(contractConfigSnapshot())
    expect(snapshot.config.http_services[0]?.routes.map(({ action }) => action.type)).toEqual([
      'proxy',
      'fixed_response',
      'redirect',
      'static_files',
    ])
  })

  it.each(['kdl', 'lua', 'uci', 'hocon'] as const)(
    'parses %s configuration source metadata without exposing native source paths',
    async (configFormat) => {
      const snapshotPayload = {
        ...emptyConfigSnapshot(),
        configFormat,
        compositional: configFormat !== 'lua',
        dependencyCount: configFormat === 'lua' ? 0 : 2,
        configPreview: `${configFormat} preview`,
        ...(configFormat === 'lua' ? { luaPreview: 'lua preview' } : {}),
        dependencies: ['/native/private/config-fragment'],
        sourcePath: '/native/private/config-root',
      }
      responseOverrides.set('GET /api/v1/config', snapshotPayload)

      const snapshot = await fetchConfig(token)

      expect(snapshot).toMatchObject({
        configFormat,
        compositional: configFormat !== 'lua',
        dependencyCount: configFormat === 'lua' ? 0 : 2,
        configPreview: `${configFormat} preview`,
      })
      expect(JSON.stringify(snapshot)).not.toContain('/native/private')
      expect(snapshot.luaPreview).toBe(configFormat === 'lua' ? 'lua preview' : undefined)

      responseOverrides.set('POST /api/v1/config/validate', {
        candidateRevision: snapshot.candidateRevision,
        normalizedConfig: snapshot.config,
        configFormat,
        compositional: snapshot.compositional,
        dependencyCount: snapshot.dependencyCount,
        configPreview: snapshot.configPreview,
        ...(configFormat === 'lua' ? { luaPreview: 'lua preview' } : {}),
        dependencies: ['/native/private/config-fragment'],
        sourcePath: '/native/private/config-root',
        diagnostics: [],
        restartRequired: false,
        topology: {
          schemaVersion: 1,
          state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1 },
          nodes: [],
          edges: [],
          overlays: [],
        },
      })

      const validation = await validateConfig(snapshot.config, token)
      expect(JSON.stringify(validation)).not.toContain('/native/private')
      expect(validation.luaPreview).toBe(configFormat === 'lua' ? 'lua preview' : undefined)
      responseOverrides.clear()
    },
  )

  it('parses exact monitoring counters and every active topology runtime state', async () => {
    const monitoring = contractMonitoring()
    monitoring.listeners = (['configured', 'listening', 'stopped', 'failed'] as const).map((state) => ({
      ...monitoring.listeners[0]!,
      name: `Listener ${state}`,
      state,
    }))
    responseOverrides.set('GET /api/v1/monitoring', monitoring)
    await expect(fetchMonitoring()).resolves.toEqual(monitoring)

    for (const runtime of ['active', 'starting', 'degraded'] as const) {
      const topology = contractTopology()
      topology.state.runtime = runtime
      topology.overlays = [
        ...(['configured', 'listening', 'stopped', 'failed'] as const).map((state) => ({
          nodeId: `listener:${state}`,
          state,
          metrics: {
            activeConnections: 0,
            acceptedConnections: '18446744073709551615',
            rejectedConnections: '0',
            bytesReceived: '18446744073709551615',
            bytesSent: '0',
          },
        })),
        ...topology.overlays.filter(({ state }) => state === 'available' || state === 'healthy'),
      ]
      responseOverrides.set('GET /api/v1/topology', topology)
      await expect(fetchTopology()).resolves.toEqual(topology)
    }
    responseOverrides.clear()
  })

  it('rejects malformed configuration and unsafe telemetry instead of exposing partial data', async () => {
    responseOverrides.set('GET /api/v1/config', {
      ...emptyConfigSnapshot(),
      config: {},
    })
    await expect(fetchConfig(token)).rejects.toThrow('invalid response payload')
    responseOverrides.delete('GET /api/v1/config')

    const missingSourceMetadata = emptyConfigSnapshot() as Partial<ReturnType<typeof emptyConfigSnapshot>>
    delete missingSourceMetadata.configPreview
    responseOverrides.set('GET /api/v1/config', missingSourceMetadata)
    await expect(fetchConfig(token)).rejects.toThrow('invalid response payload')
    responseOverrides.delete('GET /api/v1/config')

    const monitoring = contractMonitoring()
    monitoring.traffic.activeConnections = Number.MAX_SAFE_INTEGER + 1
    responseOverrides.set('GET /api/v1/monitoring', monitoring)
    await expect(fetchMonitoring()).rejects.toThrow('invalid response payload')

    const numericCounter = contractMonitoring()
    numericCounter.upstreamPools[0]!.endpoints[0]!.activeLeases = 3 as unknown as string
    responseOverrides.set('GET /api/v1/monitoring', numericCounter)
    await expect(fetchMonitoring()).rejects.toThrow('invalid response payload')

    const missingListenerState = contractMonitoring()
    delete (missingListenerState.listeners[0] as { state?: unknown }).state
    responseOverrides.set('GET /api/v1/monitoring', missingListenerState)
    await expect(fetchMonitoring()).rejects.toThrow('invalid response payload')

    const invalidListenerState = contractMonitoring()
    invalidListenerState.listeners[0]!.state = 'unknown' as typeof invalidListenerState.listeners[0]['state']
    responseOverrides.set('GET /api/v1/monitoring', invalidListenerState)
    await expect(fetchMonitoring()).rejects.toThrow('invalid response payload')

    const topology = contractTopology()
    topology.overlays[0]!.state = 'active' as unknown as typeof topology.overlays[0]['state']
    responseOverrides.set('GET /api/v1/topology', topology)
    await expect(fetchTopology()).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })
})

function route(request: IncomingMessage, response: ServerResponse): void {
  const path = request.url
  const overrideKey = `${request.method} ${path}`
  if (responseOverrides.has(overrideKey)) return json(response, responseOverrides.get(overrideKey))
  if (request.method === 'GET' && path === '/api/v1/config') return json(response, contractConfigSnapshot())
  if (request.method === 'POST' && path === '/api/v1/config/validate') {
    const snapshot = contractConfigSnapshot()
    return json(response, {
      candidateRevision: snapshot.candidateRevision,
      normalizedConfig: snapshot.config,
      configFormat: snapshot.configFormat,
      compositional: snapshot.compositional,
      dependencyCount: snapshot.dependencyCount,
      configPreview: snapshot.configPreview,
      diagnostics: [],
      restartRequired: false,
      topology: {
        schemaVersion: 1,
        state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1 },
        nodes: [],
        edges: [],
        overlays: [],
      },
    })
  }
  if (request.method === 'GET' && path === '/api/v1/monitoring') return json(response, contractMonitoring())
  if (request.method === 'GET' && path === '/api/v1/topology') return json(response, contractTopology())
  json(response, { error: { code: 'route_not_found', message: 'route does not exist' } }, 404)
}

function json(response: ServerResponse, payload: unknown, status = 200): void {
  response.writeHead(status, { 'Content-Type': 'application/json' })
  response.end(JSON.stringify(payload))
}
