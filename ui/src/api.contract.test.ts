import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

import {
  fetchConfig,
  fetchMonitoring,
} from './api'
import {
  contractConfigSnapshot,
  contractMonitoring,
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

  it('rejects malformed configuration and unsafe telemetry instead of exposing partial data', async () => {
    responseOverrides.set('GET /api/v1/config', {
      ...emptyConfigSnapshot(),
      config: {},
    })
    await expect(fetchConfig(token)).rejects.toThrow('invalid response payload')
    responseOverrides.delete('GET /api/v1/config')

    const monitoring = contractMonitoring()
    monitoring.traffic.acceptedConnections = Number.MAX_SAFE_INTEGER + 1
    responseOverrides.set('GET /api/v1/monitoring', monitoring)
    await expect(fetchMonitoring()).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })
})

function route(request: IncomingMessage, response: ServerResponse): void {
  const path = request.url
  const overrideKey = `${request.method} ${path}`
  if (responseOverrides.has(overrideKey)) return json(response, responseOverrides.get(overrideKey))
  if (request.method === 'GET' && path === '/api/v1/config') return json(response, contractConfigSnapshot())
  json(response, { error: { code: 'route_not_found', message: 'route does not exist' } }, 404)
}

function json(response: ServerResponse, payload: unknown, status = 200): void {
  response.writeHead(status, { 'Content-Type': 'application/json' })
  response.end(JSON.stringify(payload))
}
