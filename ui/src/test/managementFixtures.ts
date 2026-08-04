import type {
  GenerationResponse,
  ListenerInventoryResponse,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryResponse,
  TlsInventory,
} from '../api'
import { contractMonitoring } from './contractFixtures'

export function managementStatus(): RuntimeStatus {
  const monitoring = contractMonitoring()
  return {
    schemaVersion: 1,
    buildVersion: 'test-build',
    diskRevision: 'disk-revision',
    candidateRevision: 'candidate-revision',
    activeRevision: 'active-revision',
    previousRevision: 'previous-revision',
    degraded: false,
    listeners: monitoring.listeners,
  }
}

export function managementGeneration(): GenerationResponse {
  return {
    generation: {
      buildVersion: 'test-build',
      diskRevision: 'disk-revision',
      candidateRevision: 'candidate-revision',
      activeRevision: 'active-revision',
      previousRevision: 'previous-revision',
      quarantinedRevision: null,
      activeAccepting: true,
      degraded: false,
      lastFailure: null,
      prepares: 3,
      activations: 2,
      failures: 0,
      rollbacks: 1,
    },
  }
}

export function managementListeners(): ListenerInventoryResponse {
  return { listeners: contractMonitoring().listeners }
}

export function managementPools(): PoolInventoryResponse {
  return { pools: contractMonitoring().upstreamPools }
}

export function managementServers(): ServerInventoryResponse {
  return {
    servers: contractMonitoring().upstreamPools.flatMap((pool) => pool.endpoints.map((server) => ({
      pool: pool.name,
      server,
    }))),
  }
}

export function managementTlsInventory(): TlsInventory {
  return {
    watcher: null,
    certificates: [{
      name: 'managed-edge',
      dnsNames: ['edge.example.test'],
      source: 'acme_managed',
      developmentOnly: false,
      status: {
        certificate: 'managed-edge',
        directoryUrl: 'https://acme.example.test/directory',
        keyType: 'ecdsa_p256',
        allowedDnsSuffixes: ['example.test'],
        diskRevision: 'acme-disk',
        activeRevision: 'acme-active',
        notBeforeUnixSeconds: 1_750_000_000,
        notAfterUnixSeconds: 1_753_000_000,
        nextActionUnixSeconds: 1_752_000_000,
        notAfter: '2026-08-01T00:00:00Z',
        lastOutcome: 'activated',
        lastErrorCode: null,
      },
    }, {
      name: 'development-edge',
      dnsNames: ['localhost'],
      source: 'self_signed_development',
      developmentOnly: true,
      status: {
        activeContentRevision: 'dev-active',
        expiresAt: '2026-08-08T00:00:00Z',
        lastOutcome: null,
        lastErrorCode: null,
      },
    }],
  }
}
