import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

import {
  fetchAudit,
  fetchAuditStatus,
  fetchConfig,
  fetchEvents,
  fetchMonitoring,
  fetchRtmpCatalog,
  fetchTopology,
  setRecording,
  validateConfig,
} from './api'
import {
  contractCatalog,
  contractConfigSnapshot,
  contractMonitoring,
  contractTopology,
  emptyConfigSnapshot,
} from './test/contractFixtures'
import { durableAuditPage, durableAuditStatus } from './test/managementFixtures'

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

  it('accepts every Rust relay phase and failure emitted by catalog and monitoring APIs', async () => {
    const catalog = rustCatalogPayload()
    catalog.streams[0]!.relays[0]!.phase = 'pulling'
    catalog.streams[0]!.relays[0]!.last_failure = 'source'
    const monitoring = rustMonitoringPayload()
    monitoring.rtmp.relays[0]!.phase = 'pulling'
    monitoring.rtmp.relays[0]!.lastFailure = 'policy'
    responseOverrides.set('GET /api/v1/rtmp/streams', catalog)
    responseOverrides.set('GET /api/v1/monitoring', monitoring)

    const parsedCatalog = await fetchRtmpCatalog(undefined, token)
    const parsedMonitoring = await fetchMonitoring(undefined, token)

    expect(parsedCatalog.streams[0]!.relays[0]).toEqual(expect.objectContaining({
      phase: 'pulling',
      last_failure: 'source',
      dns_refresh_attempts: '3',
      dns_refresh_successes: '2',
      dns_refresh_failures: '1',
      last_dns_refresh_failure: 'resolution',
    }))
    expect(parsedMonitoring.rtmp).toEqual(expect.objectContaining({
      relayDnsRefreshAttempts: '3',
      relayDnsRefreshSuccesses: '2',
      relayDnsRefreshFailures: '1',
      accessLog: {
        queueCapacity: 1_024,
        queueDepth: '2',
        enqueued: '10',
        written: '8',
        dropped: '1',
        queueSaturated: '1',
        writeFailures: '0',
      },
    }))
    expect(parsedMonitoring.rtmp.relays[0]).toEqual(expect.objectContaining({
      phase: 'pulling',
      lastFailure: 'policy',
      dnsRefreshAttempts: '3',
      dnsRefreshSuccesses: '2',
      dnsRefreshFailures: '1',
      lastDnsRefreshFailure: 'address_set',
    }))
    expect(JSON.stringify([parsedCatalog, parsedMonitoring])).not.toContain('private-contract-value')
    responseOverrides.clear()
  })

  it('rejects missing, malformed, and non-string relay contract fields', async () => {
    const missingCatalogDns = rustCatalogPayload()
    delete missingCatalogDns.streams[0]!.relays[0]!.dns_refresh_attempts
    responseOverrides.set('GET /api/v1/rtmp/streams', missingCatalogDns)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')

    const arrayCatalogPhase = rustCatalogPayload()
    arrayCatalogPhase.streams[0]!.relays[0]!.phase = ['pulling']
    responseOverrides.set('GET /api/v1/rtmp/streams', arrayCatalogPhase)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')

    const objectCatalogFailure = rustCatalogPayload()
    objectCatalogFailure.streams[0]!.relays[0]!.last_failure = { value: 'source' }
    responseOverrides.set('GET /api/v1/rtmp/streams', objectCatalogFailure)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')

    const arrayCatalogDnsFailure = rustCatalogPayload()
    arrayCatalogDnsFailure.streams[0]!.relays[0]!.last_dns_refresh_failure = ['resolution']
    responseOverrides.set('GET /api/v1/rtmp/streams', arrayCatalogDnsFailure)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')

    const missingMonitoringDns = rustMonitoringPayload()
    delete missingMonitoringDns.rtmp.relayDnsRefreshAttempts
    responseOverrides.set('GET /api/v1/monitoring', missingMonitoringDns)
    await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')

    const malformedAccessLog = rustMonitoringPayload()
    const accessLog = malformedAccessLog.rtmp.accessLog as Record<string, unknown>
    accessLog.queueDepth = 2
    responseOverrides.set('GET /api/v1/monitoring', malformedAccessLog)
    await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')

    const arrayMonitoringFailure = rustMonitoringPayload()
    arrayMonitoringFailure.rtmp.relays[0]!.lastFailure = ['policy']
    responseOverrides.set('GET /api/v1/monitoring', arrayMonitoringFailure)
    await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')

    const objectMonitoringDnsFailure = rustMonitoringPayload()
    objectMonitoringDnsFailure.rtmp.relays[0]!.lastDnsRefreshFailure = { value: 'address_set' }
    responseOverrides.set('GET /api/v1/monitoring', objectMonitoringDnsFailure)
    await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })

  it('preserves the complete Rust monitoring wire shape while dropping unsupported nested fields', async () => {
    const expected = fullRustMonitoringPayload()
    const payload = structuredClone(expected)
    addMonitoringSecretCanaries(payload)
    responseOverrides.set('GET /api/v1/monitoring', payload)

    const parsed = await fetchMonitoring(undefined, token)

    expect(parsed).toEqual(expected)
    expect(JSON.stringify(parsed)).not.toContain('private-contract-value')
    responseOverrides.clear()
  })

  it('accepts Rust unsupported-platform process and host samples without fabricating values', async () => {
    const payload = fullRustMonitoringPayload()
    payload.process.status = { state: 'unsupported', reason: 'unsupported_platform' }
    payload.process.cpuPercent = null
    payload.process.residentMemoryBytes = null
    payload.process.virtualMemoryBytes = null
    payload.process.threadCount = null
    payload.process.openFileDescriptors = null
    payload.host.status = { state: 'unsupported', reason: 'unsupported_platform' }
    payload.host.loadAverage1m = null
    payload.host.loadAverage5m = null
    payload.host.loadAverage15m = null
    payload.host.totalMemoryBytes = null
    payload.host.availableMemoryBytes = null
    responseOverrides.set('GET /api/v1/monitoring', payload)

    await expect(fetchMonitoring(undefined, token)).resolves.toEqual(payload)
    responseOverrides.clear()
  })

  it('accepts the exact monitoring listener protocol vocabulary and rejects unknown strings', async () => {
    const protocols = [
      'http', 'tcp', 'rtmp', 'http3', 'udp', 'forward_http1', 'forward_http2', 'forward_http3',
    ] as const
    const payload = fullRustMonitoringPayload()
    payload.listeners = protocols.map((protocol, index) => ({
      ...payload.listeners[0]!,
      name: `listener-${index}`,
      protocol,
    }))
    responseOverrides.set('GET /api/v1/monitoring', payload)

    await expect(fetchMonitoring(undefined, token)).resolves.toEqual(payload)

    payload.listeners[0]!.protocol = 'websocket'
    await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })

  it('accepts only exact Rust recorder phases, failure codes, and notifications', async () => {
    const failureCodes = [
      'open_failed', 'write_failed', 'close_failed', 'backend_unavailable', 'file_sync_failed',
      'publish_failed', 'directory_sync_failed', 'queue_discontinuity', 'unsupported_codec',
      'shutdown_timed_out', 'worker_panicked', 'stale_publisher',
    ] as const
    const phases: Array<Record<string, unknown>> = [
      { state: 'idle' },
      { state: 'starting', operation_id: 'operation-starting' },
      { state: 'recording', operation_id: 'operation-recording', started_at_unix_ms: 1_750_000_000_000 },
      { state: 'stopping', operation_id: 'operation-stopping' },
      ...failureCodes.map((code) => ({ state: 'failed', operation_id: `operation-${code}`, code })),
    ]
    const notifications = [null, 'started', 'stopped', 'failed'] as const

    for (const [index, phase] of phases.entries()) {
      const catalog = rustCatalogPayload()
      Object.assign(catalog.streams[0]!.recorders[0]!, {
        phase,
        last_notification: notifications[index % notifications.length],
      })
      responseOverrides.set('GET /api/v1/rtmp/streams', catalog)
      const parsed = await fetchRtmpCatalog(undefined, token)
      expect(parsed.streams[0]!.recorders[0]).toMatchObject({
        phase,
        last_notification: notifications[index % notifications.length],
      })
    }

    const malformedPhases = [
      { state: 'starting' },
      { state: 'recording', operation_id: 'operation-recording' },
      { state: 'failed', operation_id: 'operation-failed', code: 'unknown_failure' },
      { state: 'idle', operation_id: 'impossible-operation' },
    ]
    for (const phase of malformedPhases) {
      const catalog = rustCatalogPayload()
      catalog.streams[0]!.recorders[0]!.phase = phase
      responseOverrides.set('GET /api/v1/rtmp/streams', catalog)
      await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')
    }

    const invalidNotification = rustCatalogPayload()
    invalidNotification.streams[0]!.recorders[0]!.last_notification = 'pending'
    responseOverrides.set('GET /api/v1/rtmp/streams', invalidNotification)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })

  it('bounds recorder command responses at the root and phase', async () => {
    const payload = structuredClone(contractCatalog().streams[0]!.recorders[1]!) as unknown as Record<string, unknown>
    payload.privateRecorder = 'private-contract-value'
    ;(payload.phase as Record<string, unknown>).privatePhase = 'private-contract-value'
    responseOverrides.set(
      'POST /api/v1/rtmp/streams/2a130dea-5db7-43e0-afb8-f07c4bcb1814/recorders/62228380-2dc6-446c-b67e-b31170b8de22/stop',
      payload,
    )

    const recorder = await setRecording(
      '2a130dea-5db7-43e0-afb8-f07c4bcb1814',
      '62228380-2dc6-446c-b67e-b31170b8de22',
      'stop',
    )

    expect(recorder).toEqual(contractCatalog().streams[0]!.recorders[1])
    expect(JSON.stringify(recorder)).not.toContain('private-contract-value')
    responseOverrides.clear()
  })

  it('rejects JSON arrays that coerce to valid primitive monitoring enums', async () => {
    const cases: Array<(payload: ReturnType<typeof fullRustMonitoringPayload>) => void> = [
      (payload) => { payload.process.administrativeState = ['ready'] },
      (payload) => { payload.process.status.state = ['healthy'] },
      (payload) => { payload.host.status.state = ['healthy'] },
      (payload) => { payload.listeners[0]!.protocol = ['http'] },
      (payload) => { payload.listeners[0]!.state = ['listening'] },
      (payload) => { payload.upstreamPools[0]!.algorithm = ['least_connections'] },
      (payload) => { payload.upstreamPools[0]!.endpoints[0]!.administrativeState = ['ready'] },
      (payload) => { payload.upstreamPools[0]!.endpoints[0]!.healthOverride = ['auto'] },
      (payload) => { payload.upstreamPools[0]!.endpoints[0]!.state = ['healthy'] },
      (payload) => { payload.upstreamPools[0]!.endpoints[1]!.lastFailure = ['connect_failed'] },
      (payload) => { payload.certbotWatcher!.health = ['degraded'] },
      (payload) => { payload.rtmp.recorders[0]!.phase = ['recording'] },
    ]

    for (const mutate of cases) {
      const payload = fullRustMonitoringPayload()
      mutate(payload)
      responseOverrides.set('GET /api/v1/monitoring', payload)
      await expect(fetchMonitoring(undefined, token)).rejects.toThrow('invalid response payload')
    }

    const catalog = rustCatalogPayload()
    const recorder = catalog.streams[0]!.recorders[0]! as Record<string, unknown>
    ;(recorder.phase as Record<string, unknown>).state = ['idle']
    responseOverrides.set('GET /api/v1/rtmp/streams', catalog)
    await expect(fetchRtmpCatalog(undefined, token)).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })

  it('requires the authoritative latest event cursor', async () => {
    responseOverrides.set('GET /api/v2/events?after=0&limit=1', {
      events: [],
      cursor: 0,
      hasMore: false,
      oldestCursor: null,
    })

    await expect(fetchEvents(0, 1, token)).rejects.toThrow('invalid response payload')
    responseOverrides.clear()
  })

  it('derives the latest cursor only when a 0.4.1 server lacks the v2 route', async () => {
    responseOverrides.set('GET /api/v1/events?after=0&limit=1', {
      events: [],
      cursor: 7,
      hasMore: false,
      oldestCursor: null,
    })

    await expect(fetchEvents(0, 1, token)).resolves.toEqual({
      events: [],
      cursor: 7,
      latestCursor: 7,
      hasMore: false,
      oldestCursor: null,
    })
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
    numericCounter.upstreamPools[0]!.endpoints[0]!.activeConnections = 3 as unknown as string
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

  it('parses durable audit pages and status with bounded fixed fields', async () => {
    const page = durableAuditPage()
    Object.assign(page.records[0]!, { endpointAddress: '10.0.0.8:443', requestQuery: 'secret=value' })
    const status = durableAuditStatus()
    responseOverrides.set('GET /api/v1/audit?after=4&limit=2&category=control&result=succeeded', page)
    responseOverrides.set('GET /api/v1/audit/status', status)

    const parsed = await fetchAudit({ after: 4, limit: 2, category: 'control', result: 'succeeded' }, token)
    expect(parsed.records).toEqual(durableAuditPage().records)
    expect(JSON.stringify(parsed)).not.toContain('10.0.0.8')
    expect(JSON.stringify(parsed)).not.toContain('secret=value')
    await expect(fetchAuditStatus(token)).resolves.toEqual(status)
    responseOverrides.clear()
  })

  it('rejects malformed durable audit records and persistence status', async () => {
    const page = durableAuditPage()
    page.records[0]!.actor = 42 as unknown as string
    responseOverrides.set('GET /api/v1/audit?after=0&limit=100', page)
    await expect(fetchAudit({ after: 0, limit: 100 }, token)).rejects.toThrow('invalid response payload')

    responseOverrides.set('GET /api/v1/audit/status', { audit: { state: 'healthy', persistent: true } })
    await expect(fetchAuditStatus(token)).rejects.toThrow('invalid response payload')
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
  if (request.method === 'GET' && path === '/api/v1/audit/status') return json(response, durableAuditStatus())
  if (request.method === 'GET' && path?.startsWith('/api/v1/audit?')) return json(response, durableAuditPage())
  json(response, { error: { code: 'route_not_found', message: 'route does not exist' } }, 404)
}

function json(response: ServerResponse, payload: unknown, status = 200): void {
  response.writeHead(status, { 'Content-Type': 'application/json' })
  response.end(JSON.stringify(payload))
}

function rustCatalogPayload(): {
  streams: Array<{
    relays: Array<Record<string, unknown>>
    recorders: Array<Record<string, unknown>>
  }>
} & Record<string, unknown> {
  const catalog = contractCatalog() as unknown as {
    streams: Array<{
      relays: Array<Record<string, unknown>>
      recorders: Array<Record<string, unknown>>
    }>
  } & Record<string, unknown>
  Object.assign(catalog.streams[0]!.relays[0]!, {
    dns_refresh_attempts: '3',
    dns_refresh_successes: '2',
    dns_refresh_failures: '1',
    last_dns_refresh_failure: 'resolution',
    privateCredential: 'private-contract-value',
  })
  Object.assign(catalog.streams[0]!.relays[0]!.destination as Record<string, unknown>, {
    privateQuery: 'private-contract-value',
  })
  catalog.privateRegistry = 'private-contract-value'
  return catalog
}

function rustMonitoringPayload(): {
  rtmp: Record<string, unknown> & { relays: Array<Record<string, unknown>> }
} & Record<string, unknown> {
  const monitoring = contractMonitoring() as unknown as {
    rtmp: Record<string, unknown> & { relays: Array<Record<string, unknown>> }
  } & Record<string, unknown>
  Object.assign(monitoring.rtmp, {
    relayDnsRefreshAttempts: '3',
    relayDnsRefreshSuccesses: '2',
    relayDnsRefreshFailures: '1',
    accessLog: {
      queueCapacity: 1_024,
      queueDepth: '2',
      enqueued: '10',
      written: '8',
      dropped: '1',
      queueSaturated: '1',
      writeFailures: '0',
      path: 'private-contract-value',
    },
    privateRegistry: 'private-contract-value',
  })
  Object.assign(monitoring.rtmp.relays[0]!, {
    dnsRefreshAttempts: '3',
    dnsRefreshSuccesses: '2',
    dnsRefreshFailures: '1',
    lastDnsRefreshFailure: 'address_set',
    privateCredential: 'private-contract-value',
  })
  monitoring.privateRuntime = 'private-contract-value'
  return monitoring
}

interface FullRustMonitoringPayload extends Record<string, unknown> {
  process: Record<string, unknown> & { status: Record<string, unknown> }
  host: Record<string, unknown> & { status: Record<string, unknown> }
  traffic: Record<string, unknown>
  listeners: Array<Record<string, unknown>>
  upstreamPools: Array<Record<string, unknown> & { endpoints: Array<Record<string, unknown>> }>
  certbotCertificates: Array<Record<string, unknown>>
  certbotWatcher: Record<string, unknown> | null
  acmeManagedCertificates: Array<Record<string, unknown>>
  directFileCertificates: Array<Record<string, unknown>>
  directFileWatcher: Record<string, unknown> | null
  transportOperations: Array<Record<string, unknown>>
  accessRecords: Array<Record<string, unknown>>
  rtmp: Record<string, unknown> & {
    relays: Array<Record<string, unknown>>
    recorders: Array<Record<string, unknown>>
  }
}

function fullRustMonitoringPayload(): FullRustMonitoringPayload {
  const monitoring = contractMonitoring() as unknown as FullRustMonitoringPayload
  monitoring.generationAgeMs = 4_000
  monitoring.process.status = { state: 'healthy' }
  monitoring.host.status = { state: 'healthy' }
  for (const listener of monitoring.listeners) {
    Object.assign(listener, {
      httpOperations: null,
      tcpRelays: null,
      proxyProtocol: null,
      cache: null,
    })
  }
  Object.assign(monitoring.listeners[0]!, {
    httpOperations: {
      outcomes: [{ result: 'success', count: '4' }, { result: 'client_error', count: '1' }],
      latency: {
        buckets: [{ upperBoundMs: 1, count: '2' }, { upperBoundMs: null, count: '5' }],
        count: '5',
        sumMs: '8',
      },
    },
    cache: { hits: '4', misses: '1', admissions: '2', evictions: '0' },
  })
  Object.assign(monitoring.listeners[1]!, {
    tcpRelays: {
      outcomes: [{ result: 'success', count: '3' }, { result: 'connect_timeout', count: '1' }],
      latency: {
        buckets: [{ upperBoundMs: 5, count: '3' }, { upperBoundMs: null, count: '4' }],
        count: '4',
        sumMs: '12',
      },
    },
    proxyProtocol: { outcomes: [{ result: 'accepted', count: '3' }] },
  })
  for (const endpoint of monitoring.upstreamPools[0]!.endpoints) {
    Object.assign(endpoint, {
      weight: 1,
      passiveEjected: false,
      passiveFailureCount: '0',
      passiveConsecutiveFailures: '0',
      passiveEjectionCount: '0',
      passiveEjectionReason: null,
      passiveEjectedAtUnixMs: null,
      passiveEjectionUntilUnixMs: null,
      passiveRecoveryCount: '0',
      passiveLastRecoveryAtUnixMs: null,
    })
  }
  monitoring.certbotCertificates = [{
    name: 'certbot-edge',
    activeArchiveRevision: 4,
    activeContentRevision: 'certbot-content',
    expiresAt: '2027-01-01T00:00:00Z',
    lastOutcome: 'unchanged',
    lastErrorCode: null,
  }]
  monitoring.acmeManagedCertificates = [{
    name: 'managed-edge',
    directoryUrl: 'https://acme.example.test/directory',
    diskRevision: 'managed-disk',
    activeRevision: 'managed-active',
    expiresAt: '2027-01-01T00:00:00Z',
    notBeforeUnixSeconds: 1_750_000_000,
    notAfterUnixSeconds: 1_780_000_000,
    nextActionUnixSeconds: 1_770_000_000,
    lastOutcome: 'activated',
    lastErrorCode: null,
    renewalInformationStatus: 'available',
    dnsProvider: 'route53',
    dnsProviderDeployment: 'healthy',
    dnsProviderHealth: 'healthy',
    dnsCleanupStatus: 'complete',
  }]
  monitoring.directFileCertificates = [{
    name: 'files-edge',
    activeContentRevision: 'files-content',
    expiresAt: '2027-01-01T00:00:00Z',
    lastOutcome: 'activated',
    lastErrorCode: null,
  }]
  monitoring.directFileWatcher = {
    health: 'healthy',
    coalescedEvents: '1',
    ignoredAccessEvents: '2',
    backendErrors: '0',
    watchRecoveries: '1',
    watchRefreshes: '2',
    rescans: '3',
    periodicRescans: '4',
    reconciliationFailures: '0',
  }
  monitoring.transportOperations = [{
    transport: 'http',
    outcomes: [{ outcome: 'success', count: '5' }, { outcome: 'client_error', count: '1' }],
    latency: {
      buckets: [{ upperBoundMs: 1, count: '3' }, { upperBoundMs: null, count: '6' }],
      count: '6',
      sumMs: '10',
    },
  }]
  monitoring.accessRecords = [{
    timestampUnixMs: 1_750_000_000_000,
    correlationId: 'request-42',
    listener: 'HTTP ingress',
    transport: 'http',
    outcome: 'success',
    durationMs: '2',
    bytesReceived: '128',
    bytesSent: '512',
  }]
  return monitoring
}

function addMonitoringSecretCanaries(payload: FullRustMonitoringPayload): void {
  const secret = 'private-contract-value'
  payload.privateRuntime = secret
  payload.process.privateProcess = secret
  payload.process.status.privateStatus = secret
  payload.host.privateHost = secret
  payload.host.status.privateStatus = secret
  payload.traffic.privateTraffic = secret
  payload.listeners[0]!.privateListener = secret
  ;(payload.listeners[0]!.httpOperations as Record<string, unknown>).privateOperations = secret
  const latency = (payload.listeners[0]!.httpOperations as Record<string, unknown>).latency as Record<string, unknown>
  latency.privateLatency = secret
  ;(latency.buckets as Array<Record<string, unknown>>)[0]!.privateBucket = secret
  payload.upstreamPools[0]!.privatePool = secret
  payload.upstreamPools[0]!.endpoints[0]!.privateEndpoint = secret
  payload.certbotCertificates[0]!.privateCertificate = secret
  payload.certbotWatcher!.privateWatcher = secret
  payload.acmeManagedCertificates[0]!.privateCertificate = secret
  payload.directFileCertificates[0]!.privateCertificate = secret
  payload.directFileWatcher!.privateWatcher = secret
  payload.transportOperations[0]!.privateOperation = secret
  ;(payload.transportOperations[0]!.outcomes as Array<Record<string, unknown>>)[0]!.privateOutcome = secret
  payload.accessRecords[0]!.privateAccess = secret
  payload.rtmp.privateRtmp = secret
  payload.rtmp.relays[0]!.privateRelay = secret
  payload.rtmp.recorders[0]!.privateRecorder = secret
}
