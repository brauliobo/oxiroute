import { isRecord, isString, safeInteger } from '../../valueGuards'
import { request } from '../transport'
import type { RestartRequired } from '../generated/controlPlane'
import type { AdministrativeState, HealthOverride, ServerTarget } from '../managementContracts'
import { invalidPayload, isCountMap, isRestartRequired, mutationHeaders } from './shared'

export interface MutationResponse {
  outcome: string
  changed?: number
  activeReferences?: number
  candidateRevision?: string
  restartRequired?: RestartRequired
}

export const WEIGHT_RANGE = { min: 1, max: 100 } as const

export interface DnsRefreshResult {
  pool: string
  server: string
  outcome: 'refreshed' | 'failed'
  addresses?: string[]
  errorCode?: string
}

export interface DnsRefreshResponse {
  outcome: 'refreshed' | 'partially_refreshed'
  atomic: false
  servers: DnsRefreshResult[]
}

export async function setListenerAdministrativeState(
  listeners: string[], state: AdministrativeState, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return postRevisionMutation(
    '/api/v1/listeners/administrative-state',
    persist ? { listeners, state, persist: true } : { listeners, state },
    expectedActiveRevision,
    token,
  )
}

export async function setPoolAdministrativeState(
  pools: string[], state: AdministrativeState, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return postRevisionMutation(
    '/api/v1/pools/administrative-state',
    persist ? { pools, state, persist: true } : { pools, state },
    expectedActiveRevision,
    token,
  )
}

export async function setServerAdministrativeState(
  targets: ServerTarget[], state: AdministrativeState, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return postRevisionMutation(
    '/api/v1/servers/administrative-state',
    persist ? { targets, state, persist: true } : { targets, state },
    expectedActiveRevision,
    token,
  )
}

export async function setServerHealthOverride(
  targets: ServerTarget[], health: HealthOverride, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return postRevisionMutation(
    '/api/v1/servers/health-override',
    persist ? { targets, health, persist: true } : { targets, health },
    expectedActiveRevision,
    token,
  )
}

export async function setServerChecks(
  targets: ServerTarget[], enabled: boolean, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return postRevisionMutation(
    '/api/v1/servers/checks',
    persist ? { targets, enabled, persist: true } : { targets, enabled },
    expectedActiveRevision,
    token,
  )
}

export async function setServerMaxConnections(
  targets: ServerTarget[], maxConnections: number | null, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return putRevisionMutation(
    '/api/v1/servers/max-connections',
    persist ? { targets, maxConnections, persist: true } : { targets, maxConnections },
    expectedActiveRevision,
    token,
  )
}

export async function setServerWeight(
  targets: ServerTarget[], weight: number | null, expectedActiveRevision: string, token: string, persist = false,
): Promise<MutationResponse> {
  return putRevisionMutation(
    '/api/v1/servers/weight',
    persist ? { targets, weight, persist: true } : { targets, weight },
    expectedActiveRevision,
    token,
  )
}

export interface EvacuationResponse {
  outcome: 'evacuated'
  force: boolean
  cancelledAndRedispatched: number
  forced: number
  leftRunning: Record<string, number>
}

export async function evacuateServers(
  targets: ServerTarget[], force: boolean, expectedActiveRevision: string, token: string,
): Promise<EvacuationResponse> {
  return postEvacuation('/api/v1/servers/evacuate', { targets, force }, expectedActiveRevision, token)
}

export async function evacuatePools(
  pools: string[], force: boolean, expectedActiveRevision: string, token: string,
): Promise<EvacuationResponse> {
  return postEvacuation('/api/v1/pools/evacuate', { pools, force }, expectedActiveRevision, token)
}

export async function refreshServerDns(
  targets: ServerTarget[], expectedActiveRevision: string, token: string,
): Promise<DnsRefreshResponse> {
  return parseDnsRefreshResponse(await request('/api/v1/servers/refresh-dns', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ targets, expectedActiveRevision }),
  }))
}

export async function reloadGeneration(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/reload', {}, expectedActiveRevision, token)
}

export async function rollbackGeneration(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/rollback', {}, expectedActiveRevision, token)
}

export async function drainGeneration(
  expectedActiveRevision: string, token: string, timeoutMs?: number,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/drain', timeoutMs === undefined ? {} : { timeoutMs }, expectedActiveRevision, token)
}

export async function drainProcess(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/process/drain', {}, expectedActiveRevision, token)
}

export async function shutdownProcess(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/process/shutdown', {}, expectedActiveRevision, token)
}

async function postRevisionMutation(
  url: string, body: Record<string, unknown>, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return parseMutationResponse(await request(url, {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ ...body, expectedActiveRevision }),
  }))
}

async function postEvacuation(
  url: string, body: Record<string, unknown>, expectedActiveRevision: string, token: string,
): Promise<EvacuationResponse> {
  return parseEvacuationResponse(await request(url, {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ ...body, expectedActiveRevision }),
  }))
}

async function putRevisionMutation(
  url: string, body: Record<string, unknown>, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return parseMutationResponse(await request(url, {
    method: 'PUT', headers: mutationHeaders(token),
    body: JSON.stringify({ ...body, expectedActiveRevision }),
  }))
}

function parseMutationResponse(value: unknown): MutationResponse {
  if (!isRecord(value) || typeof value.outcome !== 'string' ||
    (value.changed !== undefined && !safeInteger(value.changed)) ||
    (value.activeReferences !== undefined && !safeInteger(value.activeReferences)) ||
    (value.candidateRevision !== undefined && !isString(value.candidateRevision)) ||
    (value.restartRequired !== undefined && !isRestartRequired(value.restartRequired))
  ) return invalidPayload('management mutation')
  return {
    outcome: value.outcome,
    ...(value.changed === undefined ? {} : { changed: value.changed }),
    ...(value.activeReferences === undefined ? {} : { activeReferences: value.activeReferences }),
    ...(value.candidateRevision === undefined ? {} : { candidateRevision: value.candidateRevision }),
    ...(value.restartRequired === undefined ? {} : { restartRequired: value.restartRequired }),
  }
}

function parseEvacuationResponse(value: unknown): EvacuationResponse {
  if (!isRecord(value) || value.outcome !== 'evacuated' || typeof value.force !== 'boolean' ||
    !safeInteger(value.cancelledAndRedispatched) || !safeInteger(value.forced) ||
    !isCountMap(value.leftRunning)
  ) return invalidPayload('evacuation')
  return {
    outcome: 'evacuated',
    force: value.force,
    cancelledAndRedispatched: value.cancelledAndRedispatched,
    forced: value.forced,
    leftRunning: value.leftRunning,
  }
}

function parseDnsRefreshResponse(value: unknown): DnsRefreshResponse {
  if (!isRecord(value) || !['refreshed', 'partially_refreshed'].includes(String(value.outcome)) ||
    value.atomic !== false || !Array.isArray(value.servers) || !value.servers.every(dnsRefreshResult)
  ) return invalidPayload('DNS refresh')
  return {
    outcome: value.outcome as DnsRefreshResponse['outcome'],
    atomic: false,
    servers: value.servers.map((server) => normalizeDnsRefreshResult(server as Record<string, unknown>)),
  }
}

function dnsRefreshResult(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && typeof value.pool === 'string' && typeof value.server === 'string' &&
    ['refreshed', 'failed'].includes(String(value.outcome)) &&
    (value.addresses === undefined || (Array.isArray(value.addresses) && value.addresses.every((address) => typeof address === 'string'))) &&
    (value.error === undefined || (isRecord(value.error) && typeof value.error.code === 'string'))
}

function normalizeDnsRefreshResult(value: Record<string, unknown>): DnsRefreshResult {
  return {
    pool: value.pool as string,
    server: value.server as string,
    outcome: value.outcome as DnsRefreshResult['outcome'],
    ...(value.addresses === undefined ? {} : { addresses: value.addresses as string[] }),
    ...(value.error === undefined ? {} : { errorCode: (value.error as Record<string, unknown>).code as string }),
  }
}
