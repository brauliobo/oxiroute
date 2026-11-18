import { arrayOf, decimalString, isRecord, isString, nullableSafeInteger, nullableString, safeInteger } from '../../valueGuards'
import type { InflightView, Phase, RequestListResponse } from '../generated/controlPlane'
import { request } from '../transport'
import { authorizationHeader, invalidPayload } from './shared'

export type InflightPhase = Phase
export type InflightRequest = InflightView
export type InflightRequestList = RequestListResponse

export const INFLIGHT_PHASES: readonly InflightPhase[] =
  ['routed', 'queued', 'connecting', 'awaiting_headers', 'streaming', 'retry_wait']

export const INFLIGHT_LIMIT_MAX = 1000

export interface InflightRequestFilter {
  pool?: string
  route?: string
  phase?: InflightPhase
  minAgeMs?: number
  limit?: number
  requestId?: string
}

export async function fetchInflightRequests(
  token: string, filter: InflightRequestFilter = {}, signal?: AbortSignal,
): Promise<InflightRequestList> {
  const query = inflightQuery(filter)
  return parseInflightRequestList(await request(`/api/v1/requests${query && `?${query}`}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

function inflightQuery(filter: InflightRequestFilter): string {
  const query = new URLSearchParams()
  if (filter.pool) query.set('pool', filter.pool)
  if (filter.route) query.set('route', filter.route)
  if (filter.phase) query.set('phase', filter.phase)
  if (filter.minAgeMs) query.set('min_age_ms', String(filter.minAgeMs))
  if (filter.limit) query.set('limit', String(filter.limit))
  if (filter.requestId) query.set('requestId', filter.requestId)
  return query.toString()
}

function inflightRequest(value: unknown): value is InflightRequest {
  return isRecord(value) && decimalString(value.ageMs) && safeInteger(value.attempts) &&
    nullableString(value.clientIp) && nullableString(value.host) && isString(value.listener) &&
    isString(value.method) && typeof value.overdue === 'boolean' && isString(value.path) &&
    INFLIGHT_PHASES.includes(value.phase as InflightPhase) && nullableString(value.pool) &&
    (value.queuePosition === undefined || nullableSafeInteger(value.queuePosition)) &&
    isString(value.requestId) && nullableString(value.route) && nullableString(value.server) &&
    safeInteger(value.startedAtUnixMs) &&
    (value.waitedMs === undefined || value.waitedMs === null || decimalString(value.waitedMs))
}

function parseInflightRequestList(value: unknown): InflightRequestList {
  if (!isRecord(value) || !arrayOf(value.requests, inflightRequest) || !decimalString(value.total) ||
    typeof value.truncated !== 'boolean'
  ) return invalidPayload('in-flight request list')
  return { requests: value.requests, total: value.total, truncated: value.truncated }
}
