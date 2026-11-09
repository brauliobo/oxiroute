import type {
  DecimalCounter,
  Histogram,
  ListenerStats,
  PoolStats,
  ServerStats,
  StatsDocument,
} from './generated/statsPage'

// Invented data shaped by the published contract (contracts/stats-page.openapi.json): pools `web` and `api`, servers
// `origin-a` .. `origin-c`, documentation address ranges. Used by the unit tests, the browser smoke test and Phase 4.
// Every function returns fresh objects, so callers may mutate what they get.

export const OPERATION_BOUNDS_MS = [1, 5, 10, 50, 100, 500, 1000, 5000]
export const QUEUE_BOUNDS_MS = [1, 10, 100, 1000]
export const BASE_SAMPLED_AT_MS = 1_800_000_000_000

export const counter = (value: number | bigint): DecimalCounter => `${value}` as DecimalCounter

/** A cumulative histogram of the given observations (milliseconds) over `bounds`; the overflow is `count`. */
export function histogram(bounds: readonly number[], observations: readonly number[]): Histogram {
  const buckets = bounds.map((bound) => counter(observations.filter((value) => value <= bound).length))
  const sum = observations.reduce((total, value) => total + value, 0)
  return { buckets, count: counter(observations.length), sumMs: counter(sum) }
}

const operationHistogram = (observations: readonly number[]) => histogram(OPERATION_BOUNDS_MS, observations)
const queueHistogram = (observations: readonly number[]) => histogram(QUEUE_BOUNDS_MS, observations)

export function wireServer(name: string, address: string, overrides: Partial<ServerStats> = {}): ServerStats {
  return {
    name,
    address,
    origin: 'static',
    status: 'up',
    available: true,
    administrativeState: 'ready',
    healthOverride: 'auto',
    health: 'healthy',
    weight: 1,
    configuredWeight: 1,
    maxConnections: 100,
    configuredMaxConnections: 100,
    lastTransitionAtUnixMs: BASE_SAMPLED_AT_MS - 600_000,
    check: {
      enabled: true,
      running: true,
      successful: counter(120),
      failed: counter(0),
      consecutiveSuccesses: 120,
      consecutiveFailures: 0,
      lastCheckedAtUnixMs: BASE_SAMPLED_AT_MS - 2_000,
    },
    passive: {
      ejected: false,
      consecutiveFailures: 0,
      failureCount: counter(0),
      ejectionCount: counter(0),
      recoveryCount: counter(0),
    },
    traffic: {
      sessions: { current: 3, total: counter(1000) },
      bytesReceived: counter(2_000_000),
      bytesSent: counter(500_000),
      responses: { '1xx': counter(0), '2xx': counter(900), '3xx': counter(10), '4xx': counter(40), '5xx': counter(20), none: counter(30) },
      attempts: {
        ok: counter(950), connectFailed: counter(10), timeout: counter(5), protocolError: counter(5),
        statusRetry: counter(10), rescued: counter(0), clientClosed: counter(20), evacuated: counter(0),
      },
      evacuated: { redispatched: counter(0), failed: counter(0), forced: counter(0), leftRunning: counter(0) },
    },
    latency: {
      request: operationHistogram([2, 4, 8, 40, 40, 90, 200, 450, 900, 4000]),
      header: operationHistogram([1, 3, 6, 30, 30, 80, 150, 400, 800, 3000]),
    },
    ...overrides,
  }
}

export function wirePool(name: string, servers: ServerStats[], overrides: Partial<PoolStats> = {}): PoolStats {
  const available = servers.filter((server) => server.available).length
  const sum = (pick: (server: ServerStats) => bigint) => counter(servers.reduce((total, server) => total + pick(server), 0n))
  return {
    name,
    algorithm: 'round_robin',
    status: available === servers.length ? 'up' : available === 0 ? 'down' : 'degraded',
    availableServers: available,
    totalServers: servers.length,
    failOpenSelections: counter(0),
    unavailableSelections: counter(0),
    inflight: { requests: 3, oldestAgeMs: 120 },
    queue: {
      current: 0,
      oldestWaitMs: 0,
      total: counter(4),
      timeouts: counter(0),
      cancellations: counter(0),
      wait: queueHistogram([0, 5, 50, 500]),
    },
    traffic: {
      ...servers[0]!.traffic,
      sessions: {
        current: servers.reduce((total, server) => total + server.traffic.sessions.current, 0),
        total: sum((server) => BigInt(server.traffic.sessions.total)),
      },
      bytesReceived: sum((server) => BigInt(server.traffic.bytesReceived)),
      bytesSent: sum((server) => BigInt(server.traffic.bytesSent)),
    },
    latency: servers[0]!.latency,
    servers,
    ...overrides,
  }
}

export function wireListener(name: string, overrides: Partial<ListenerStats> = {}): ListenerStats {
  return {
    name,
    protocol: 'http',
    bind: 'socket:192.0.2.10:8080',
    service: 'site',
    state: 'listening',
    administrativeState: 'ready',
    activeConnections: 7,
    maxConnections: 1000,
    acceptedConnections: counter(5000),
    rejectedConnections: counter(2),
    bytesReceived: counter(8_000_000),
    bytesSent: counter(64_000_000),
    http: {
      total: counter(4900),
      outcomes: [
        { result: 'success', count: counter(4800) },
        { result: 'client_error', count: counter(60) },
        { result: 'upstream_error', count: counter(30) },
        { result: 'timeout', count: counter(10) },
      ],
      latency: operationHistogram([2, 4, 8, 40, 40, 90, 200, 450, 900, 4000]),
    },
    ...overrides,
  }
}

/** A complete wire document: listener `public-http`, pools `web` (two servers) and `api` (one), one route. */
export function wireDocument(overrides: Partial<StatsDocument> = {}): StatsDocument {
  const web = wirePool('web', [
    wireServer('origin-a', '198.51.100.11:8080'),
    wireServer('origin-b', '198.51.100.12:8080', { status: 'drain', administrativeState: 'drain' }),
  ])
  const api = wirePool('api', [wireServer('origin-c', '198.51.100.21:9000', { origin: 'dynamic' })])
  return {
    schemaVersion: 1,
    sampledAtUnixMs: BASE_SAMPLED_AT_MS,
    intervalHintMs: 2000,
    admin: false,
    bucketBoundsMs: { operation: [...OPERATION_BOUNDS_MS], queueWait: [...QUEUE_BOUNDS_MS] },
    process: {
      version: '6.3.0',
      state: 'healthy',
      administrativeState: 'ready',
      uptimeMs: 3_600_000,
      activeConnections: 7,
      maxConnections: 4096,
      acceptedConnections: counter(5000),
      rejectedConnections: counter(2),
      bytesReceived: counter(8_000_000),
      bytesSent: counter(64_000_000),
      retryAttempts: counter(10),
      cpuPercent: 12.5,
      residentMemoryBytes: 150_000_000,
      virtualMemoryBytes: 900_000_000,
      threadCount: 8,
      openFileDescriptors: 64,
      inflight: { upstreamRequests: 3, oldestUpstreamAgeMs: 120, overdue: 0, stuck: 0 },
      host: { state: 'healthy', loadAverage1m: 0.5, loadAverage5m: 0.4, loadAverage15m: 0.3, totalMemoryBytes: 8_000_000_000, availableMemoryBytes: 4_000_000_000 },
    },
    generation: {
      number: 4,
      ageMs: 600_000,
      accepting: true,
      degraded: false,
      activeRevision: 'rev-4',
      previousRevision: 'rev-3',
      activations: counter(4),
      prepares: counter(5),
      rollbacks: counter(0),
      failures: counter(1),
      configRejections: counter(0),
      maxRetiredGenerations: 4,
      retired: [],
    },
    listeners: [wireListener('public-http')],
    routes: [{
      id: 'site#0',
      service: 'site',
      index: 0,
      listeners: ['public-http'],
      host: { kind: 'normalized_host', value: 'example.com' },
      path: { kind: 'segment_prefix', value: '/' },
      methods: [],
      action: 'proxy',
      pool: 'web',
      maxRetries: 1,
    }],
    pools: [web, api],
    certificates: [{
      name: 'edge',
      source: 'direct_file',
      expiresAt: 'Dec 31 23:59:59 2026 GMT',
      notAfterUnixSeconds: 1_798_761_599,
    }],
    certificateWatchers: [],
    warnings: [{ code: 'W_PASSIVE_HEALTH_MISSING', message: 'Pool api has no passive health settings.' }],
    ...overrides,
  }
}
