import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  fetchGenerations,
  fetchListeners,
  fetchMonitoring,
  fetchPools,
  fetchServers,
} from './api'
import {
  contractMonitoring,
  jsonResponse,
} from './test/contractFixtures'
import {
  managementGeneration,
  managementListeners,
  managementPools,
  managementServers,
} from './test/managementFixtures'

const token = 'phase6-ui-management-token'
const secretCanary = 'phase6-ui-secret-canary'
type JsonRecord = Record<string, unknown>

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('Phase 6 management response characterization', () => {
  it('parses the generation and inventory DTO envelopes with decimal counters and nulls intact', async () => {
    const expectedGeneration = managementGeneration()
    stubJson(expectedGeneration)
    const generation = await fetchGenerations(token)
    expect(generation).toEqual(expectedGeneration)
    expect(generation.generation.quarantinedRevision).toBeNull()

    const expectedListeners = managementListeners()
    stubJson(expectedListeners)
    const listeners = await fetchListeners(token)
    expect(listeners).toEqual(expectedListeners)
    expect(listeners.listeners[1]?.maxConnections).toBeNull()
    expect(listeners.listeners[1]?.httpOperations).toBeNull()
    expect(listeners.listeners[1]?.tcpRelays).toBeNull()
    expect(listeners.listeners[1]?.proxyProtocol).toBeNull()
    expect(listeners.listeners[1]?.cache).toBeNull()

    const expectedPools = managementPools()
    stubJson(expectedPools)
    const pools = await fetchPools(token)
    expect(pools).toEqual(expectedPools)
    expect(pools.pools[0]?.unavailableSelections).toBe('18446744073709551615')
    expect(pools.pools[0]?.endpoints[1]?.configuredMaxConnections).toBeNull()
    expect(pools.pools[0]?.endpoints[1]?.maxConnections).toBeNull()
    expect(pools.pools[0]?.endpoints[1]?.lastFailure).toBe('connect_failed')

    const expectedServers = managementServers()
    stubJson(expectedServers)
    const servers = await fetchServers(token)
    expect(servers).toEqual(expectedServers)
    expect(servers.servers).toHaveLength(2)
    expect(servers.servers[0]?.server.successfulChecks).toBe('18446744073709551615')
    expect(servers.servers[1]?.server.passiveEjectionReason).toBeNull()
  })

  it('accepts every closed management enum vocabulary', async () => {
    for (const administrativeState of ['ready', 'drain', 'maintenance'] as const) {
      const payload = managementListeners()
      payload.listeners[0]!.administrativeState = administrativeState
      stubJson(payload)
      await expect(fetchListeners(token)).resolves.toEqual(payload)
    }

    for (const state of ['configured', 'listening', 'stopped', 'failed'] as const) {
      const payload = managementListeners()
      payload.listeners[0]!.state = state
      stubJson(payload)
      await expect(fetchListeners(token)).resolves.toEqual(payload)
    }

    for (const protocol of [
      'http', 'tcp', 'rtmp', 'http3', 'udp', 'forward_http1', 'forward_http2', 'forward_http3',
    ] as const) {
      const payload = managementListeners()
      payload.listeners[0]!.protocol = protocol
      stubJson(payload)
      await expect(fetchListeners(token)).resolves.toEqual(payload)
    }

    for (const algorithm of ['first', 'round_robin', 'least_connections', 'weighted_round_robin'] as const) {
      const payload = managementPools()
      payload.pools[0]!.algorithm = algorithm
      stubJson(payload)
      await expect(fetchPools(token)).resolves.toEqual(payload)
    }

    for (const state of ['unchecked', 'unknown', 'healthy', 'unhealthy'] as const) {
      const payload = managementServers()
      payload.servers[0]!.server.state = state
      stubJson(payload)
      await expect(fetchServers(token)).resolves.toEqual(payload)
    }

    for (const healthOverride of ['auto', 'up', 'down'] as const) {
      const payload = managementServers()
      payload.servers[0]!.server.healthOverride = healthOverride
      stubJson(payload)
      await expect(fetchServers(token)).resolves.toEqual(payload)
    }

    for (const failure of ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'] as const) {
      const payload = managementServers()
      payload.servers[0]!.server.lastFailure = failure
      stubJson(payload)
      await expect(fetchServers(token)).resolves.toEqual(payload)
    }
  })

  it('rejects missing fields, numeric counters, arrays, and unknown enums', async () => {
    const missingGeneration = managementGeneration() as unknown as JsonRecord
    delete (missingGeneration.generation as JsonRecord).quarantinedRevision
    stubJson(missingGeneration)
    await expect(fetchGenerations(token)).rejects.toThrow('invalid response payload')

    const numericGenerationCounter = managementGeneration()
    numericGenerationCounter.generation.prepares = '3' as unknown as number
    stubJson(numericGenerationCounter)
    await expect(fetchGenerations(token)).rejects.toThrow('invalid response payload')

    const missingListenerState = managementListeners() as unknown as JsonRecord
    delete ((missingListenerState.listeners as Array<JsonRecord>)[0]!).state
    stubJson(missingListenerState)
    await expect(fetchListeners(token)).rejects.toThrow('invalid response payload')

    const arrayListenerProtocol = managementListeners()
    arrayListenerProtocol.listeners[0]!.protocol = ['http'] as unknown as typeof arrayListenerProtocol.listeners[0]['protocol']
    stubJson(arrayListenerProtocol)
    await expect(fetchListeners(token)).rejects.toThrow('invalid response payload')

    const unknownPoolAlgorithm = managementPools()
    unknownPoolAlgorithm.pools[0]!.algorithm = 'random' as typeof unknownPoolAlgorithm.pools[0]['algorithm']
    stubJson(unknownPoolAlgorithm)
    await expect(fetchPools(token)).rejects.toThrow('invalid response payload')

    const numericPoolCounter = managementPools()
    numericPoolCounter.pools[0]!.endpoints[0]!.activeConnections = 3 as unknown as string
    stubJson(numericPoolCounter)
    await expect(fetchPools(token)).rejects.toThrow('invalid response payload')

    const missingServer = managementServers() as unknown as JsonRecord
    delete ((missingServer.servers as Array<JsonRecord>)[0]!).server
    stubJson(missingServer)
    await expect(fetchServers(token)).rejects.toThrow('invalid response payload')

    const unknownFailure = managementServers()
    unknownFailure.servers[0]!.server.lastFailure = 'tls' as typeof unknownFailure.servers[0]['server']['lastFailure']
    stubJson(unknownFailure)
    await expect(fetchServers(token)).rejects.toThrow('invalid response payload')
  })

  it('projects monitoring canaries without retaining unsupported secret-bearing fields', async () => {
    const payload = contractMonitoring() as unknown as JsonRecord
    const listener = (payload.listeners as Array<JsonRecord>)[0]!
    const pool = (payload.upstreamPools as Array<JsonRecord>)[0]!
    const endpoint = (pool.endpoints as Array<JsonRecord>)[0]!
    const rtmp = payload.rtmp as JsonRecord
    payload.privateRuntime = secretCanary
    listener.privateListener = secretCanary
    pool.privatePool = secretCanary
    endpoint.privateEndpoint = secretCanary
    rtmp.privateRtmp = secretCanary

    stubJson(payload)
    const parsed = await fetchMonitoring()

    expect(JSON.stringify(parsed)).not.toContain(secretCanary)
    expect(parsed.listeners[0]).not.toHaveProperty('privateListener')
    expect(parsed.upstreamPools[0]).not.toHaveProperty('privatePool')
    expect(parsed.upstreamPools[0]?.endpoints[0]).not.toHaveProperty('privateEndpoint')
    expect(parsed.rtmp).not.toHaveProperty('privateRtmp')
  })
})

function stubJson(payload: unknown): void {
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse(payload))))
}
