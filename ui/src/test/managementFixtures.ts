import type {
  AuditPage,
  AuditStatusResponse,
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
  const audit = {
    state: 'healthy' as const,
    persistent: true,
    degraded: false,
    recordCount: 12,
    bytes: 4_096,
    rotatedFiles: 1,
    maxRecords: 10_000,
    maxRecordBytes: 4_096,
    maxFileBytes: 1_048_576,
    maxTotalBytes: 16_777_216,
    maxRotatedFiles: 8,
    writeFailures: 0,
    corruptRecords: 0,
  }
  const h3 = {
    status: 'unconfigured' as const,
    supported: true,
    listeners: [],
    configuredListeners: [],
    transport: 'quic' as const,
    alpn: ['h3'],
    tlsMinVersion: '1.3',
    zeroRtt: 'disabled' as const,
    migration: 'disabled' as const,
    goAway: 'graceful' as const,
    fallback: 'none' as const,
    unsupported: [],
    limits: {
      maxHandshakesAndConnections: 128,
      maxBidirectionalStreams: 128,
      maxUnidirectionalStreams: 16,
      maxFieldSectionBytes: 65_536,
      maxRequestBodyBytes: 1_048_576,
      maxResponseBodyBytes: 1_048_576,
    },
    blockedReason: null,
  }
  return {
    schemaVersion: 1,
    buildVersion: 'test-build',
    diskRevision: 'disk-revision',
    candidateRevision: 'candidate-revision',
    activeRevision: 'active-revision',
    previousRevision: 'previous-revision',
    degraded: false,
    activeGenerationAgeMs: 0,
    components: {
      process: { state: 'healthy' as const, reason: null },
      host: { state: 'healthy' as const, reason: null },
      generation: { state: 'healthy' as const, reason: null },
      audit,
    },
    certificates: {
      certbot: [],
      acmeManaged: [],
      directFiles: [],
    },
    audit,
    capabilities: {
      schemaVersion: 1,
      supervision: {
        mode: 'direct' as const,
        descriptorAdoption: {
          status: 'not_used' as const,
          manifestVersion: 1,
          datagram: false,
          quic: false,
        },
      },
      udp: {
        status: 'unconfigured' as const,
        supported: false,
        listeners: [],
        configuredListeners: [],
        transport: 'udp' as const,
        drain: 'graceful' as const,
        fallback: 'none' as const,
        blockedReason: null,
      },
      http3: { reverse: h3, forward: h3 },
    },
    listeners: monitoring.listeners,
    tlsProfiles: [],
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
        challenge: 'http01',
        dnsProvider: null,
        keyType: 'ecdsa_p256',
        allowedDnsSuffixes: ['example.test'],
        diskRevision: 'acme-disk',
        activeRevision: 'acme-active',
        notBeforeUnixSeconds: 1_750_000_000,
        notAfterUnixSeconds: 1_753_000_000,
        nextActionUnixSeconds: 1_752_000_000,
        notAfter: '2026-08-01T00:00:00Z',
        jobStatus: 'succeeded',
        jobId: null,
        paused: false,
        retainedRevisions: 3,
        retentionDays: 30,
        retryAttempt: 0,
        lastSuccessUnixSeconds: 1_751_000_000,
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

export function durableAuditPage(): AuditPage {
  return {
    records: [{
      id: 12,
      timestampUnixMs: 1_754_000_000_000,
      correlationId: 'corr-redacted-12',
      actor: 'actor-redacted',
      source: 'management_api',
      category: 'control',
      operation: 'server_update',
      result: 'succeeded',
      revision: 'active-revision',
    }],
    cursor: 12,
    latestCursor: 12,
    hasMore: false,
    oldestCursor: 12,
  }
}

export function durableAuditStatus(): AuditStatusResponse {
  return {
    audit: {
      state: 'healthy',
      persistent: true,
      degraded: false,
      recordCount: 12,
      bytes: 4_096,
      rotatedFiles: 1,
      maxRecords: 10_000,
      maxRecordBytes: 4_096,
      maxFileBytes: 1_048_576,
      maxTotalBytes: 16_777_216,
      maxRotatedFiles: 8,
      writeFailures: 0,
      corruptRecords: 0,
    },
  }
}
