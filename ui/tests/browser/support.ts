import type { Page, Request } from '@playwright/test'

import type { CanonicalConfig, ConfigSnapshot } from '../../src/config'
import {
  contractCatalog,
  contractConfigSnapshot,
  contractMonitoring,
  contractRtmpStats,
  contractTopology,
  emptyConfigSnapshot,
} from '../../src/test/contractFixtures'

export const MANAGEMENT_TOKEN = 'browser-test-management-token'
export const CONFIG_TOKEN = 'browser-test-config-token'

export type JsonMockResponse = {
  kind?: 'json'
  status?: number
  body: unknown
}

export type SseMockResponse = {
  kind: 'sse'
  body: string
}

export type ApiMockResponse = JsonMockResponse | SseMockResponse
export type ApiMockHandler = (request: Request) => ApiMockResponse | Promise<ApiMockResponse | undefined> | undefined

export async function installApiMock(page: Page, handler: ApiMockHandler): Promise<void> {
  const localOrigin = new URL('http://127.0.0.1:4173').origin
  await page.route('**/*', async (route) => {
    const url = new URL(route.request().url())
    if (url.origin !== localOrigin) {
      await route.abort('blockedbyclient')
      return
    }
    if (!url.pathname.startsWith('/api/v1/')) {
      await route.continue()
      return
    }

    const response = await handler(route.request())
    if (!response) {
      await route.abort('failed')
      return
    }
    if (response.kind === 'sse') {
      await route.fulfill({
        status: 200,
        headers: {
          'Cache-Control': 'no-cache',
          'Content-Type': 'text/event-stream; charset=utf-8',
        },
        body: response.body,
      })
      return
    }
    await route.fulfill({
      status: response.status ?? 200,
      contentType: 'application/json',
      body: JSON.stringify(response.body),
    })
  })
}

export function json(body: unknown, status = 200): JsonMockResponse {
  return { body, status }
}

export function sse(body: string): SseMockResponse {
  return { body, kind: 'sse' }
}

export function requestPath(request: Request): string {
  return new URL(request.url()).pathname
}

export function requestBody<T>(request: Request): T {
  return JSON.parse(request.postData() ?? '{}') as T
}

export function dashboardResponse(path: string): ApiMockResponse | undefined {
  if (path === '/api/v1/monitoring') return json(contractMonitoring())
  if (path === '/api/v1/rtmp/streams') return json(contractCatalog())
  if (path === '/api/v1/rtmp/stats') return json(contractRtmpStats())
  if (path === '/api/v1/topology') return json(contractTopology())
  return undefined
}

export function configSnapshot(revision = 'disk-revision', version = 1): ConfigSnapshot {
  const snapshot = emptyConfigSnapshot()
  const config = structuredClone(snapshot.config)
  config.version = version
  return {
    ...snapshot,
    activeRevision: revision,
    candidateRevision: revision,
    config,
    diskRevision: revision,
  }
}

export function managedAcmeConfigSnapshot(): ConfigSnapshot {
  const snapshot = configSnapshot()
  snapshot.config.certificates = [{
    name: 'managed-edge',
    dns_names: ['edge.example.test'],
    source: {
      type: 'acme_managed',
      directory_url: 'https://acme.example.test/directory',
      state_root: '/var/lib/oxiroute/acme',
      contacts: ['mailto:ops@example.test'],
      terms_agreed: true,
      challenge: 'http01',
      key_type: 'ecdsa_p256',
      allowed_dns_suffixes: ['example.test'],
      retained_revisions: 3,
      retention_days: 30,
      dns01: null,
    },
  }]
  return snapshot
}

export function configValidation(config: CanonicalConfig): Record<string, unknown> {
  return {
    candidateRevision: 'candidate-revision',
    normalizedConfig: structuredClone(config),
    configFormat: 'kdl',
    compositional: false,
    dependencyCount: 0,
    configPreview: `version ${config.version}\n`,
    diagnostics: [],
    restartRequired: false,
    topology: {
      schemaVersion: 1,
      state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1_750_000_000_000 },
      nodes: [],
      edges: [],
      overlays: [],
    },
  }
}

export function configSaveResponse(): Record<string, unknown> {
  return {
    diskRevision: 'saved-disk-revision',
    candidateRevision: 'saved-candidate-revision',
    activeRevision: 'active-revision',
    outcome: 'saved_pending_activation',
    activationState: 'pending',
    restartRequired: false,
    diagnostics: [],
  }
}

export function managementEventPage() {
  return {
    events: [{
      cursor: 4,
      timestampUnixMs: null,
      event: 'certificate_renewal',
      outcome: 'requested',
      revision: 'active-revision',
      certificate: 'managed-edge',
    }],
    cursor: 4,
    latestCursor: 4,
    hasMore: false,
    oldestCursor: 4,
  }
}

export function shutdownStream(): SseMockResponse {
  return sse('event: ready\ndata: {"cursor":0}\n\n' +
    'event: shutdown\ndata: {"reason":"server_shutdown"}\n\n')
}

export function managementMonitoring(): ApiMockResponse {
  return json({
    ...contractMonitoring(),
    listeners: [],
    upstreamPools: [],
  })
}

export function configFixtureWithDiagnostic(): ConfigSnapshot {
  const snapshot = contractConfigSnapshot()
  snapshot.diagnostics = [{
    code: 'W_IMPORT_PROVENANCE',
    severity: 'warning',
    stage: 'parse',
    path: '/native_sources/0',
    message: 'Native import details are available through the offline report command.',
  }]
  return snapshot
}
