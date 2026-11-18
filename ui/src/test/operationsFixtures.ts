import type { InflightRequestList, MonitoringPool, PoolInventoryResponse, RegistrationList, ServerInventoryResponse } from '../api'
import { managementPools } from './managementFixtures'

export function operationsInventory(): { pools: PoolInventoryResponse; servers: ServerInventoryResponse } {
  const pools = managementPools()
  const web = pools.pools[0]!
  const base = web.endpoints[0]!
  const staticPool: MonitoringPool = {
    ...web,
    name: 'static',
    algorithm: 'round_robin',
    availableEndpoints: 1,
    totalEndpoints: 1,
    queued: 0,
    endpoints: [{
      ...base,
      name: 'static-1',
      address: 'origin.example.com:8080',
      state: 'unchecked',
      checksRunning: false,
      activeConnections: '0',
      weight: 5,
      maxConnections: null,
    }],
  }
  pools.pools.push(staticPool)
  const servers = pools.pools.flatMap((pool) => pool.endpoints.map((server) => ({ pool: pool.name, server })))
  return { pools, servers: { servers } }
}

export function inflightList(): InflightRequestList {
  return {
    total: '2',
    truncated: false,
    requests: [{
      requestId: 'rq-old',
      ageMs: '42000',
      attempts: 2,
      clientIp: '192.0.2.10',
      host: 'app.example.com',
      listener: 'HTTP ingress',
      method: 'POST',
      overdue: true,
      path: '/v1/render',
      phase: 'awaiting_headers',
      pool: 'web-backends',
      route: 'render',
      server: 'primary',
      startedAtUnixMs: 1_750_000_000_000,
    }, {
      requestId: 'rq-new',
      ageMs: '120',
      attempts: 1,
      clientIp: null,
      host: null,
      listener: 'HTTP ingress',
      method: 'GET',
      overdue: false,
      path: '/health',
      phase: 'queued',
      pool: 'web-backends',
      route: null,
      server: null,
      startedAtUnixMs: 1_750_000_041_880,
      queuePosition: 1,
      waitedMs: '120',
    }],
  }
}

export function registrationList(): RegistrationList {
  return {
    registryRevision: 'registry-revision',
    writer: true,
    degraded: null,
    registrations: [{
      status: 'active',
      expiresInMs: 25_000,
      registration: {
        pool: 'web-backends',
        name: 'worker-a',
        address: '192.0.2.21:9000',
        weight: 10,
        priority: 0,
        maxConnections: 50,
        labels: { zone: 'a' },
        metadata: { build: '1' },
        ttlMs: 30_000,
        announcedState: 'ready',
        operatorState: 'ready',
        effectiveState: 'ready',
        registeredBy: 'agent',
      },
    }, {
      status: 'shadowed',
      expiresInMs: null,
      registration: {
        pool: 'web-backends',
        name: 'worker-b',
        address: '192.0.2.22:9000',
        weight: 1,
        priority: 0,
        maxConnections: null,
        labels: {},
        metadata: null,
        ttlMs: 30_000,
        announcedState: 'draining',
        operatorState: 'ready',
        effectiveState: 'drain',
        registeredBy: 'agent',
      },
    }],
  }
}
