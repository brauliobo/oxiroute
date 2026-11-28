import { isRecord } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, invalidPayload } from './shared'

export interface RouteExplainQuery {
  path: string
  host?: string
  method?: string
  listener?: string
}

export interface RouteExplainMatch {
  listener: string
  service: string
  routeIndex: number
  action: string
  pool?: string | null
  detail?: Record<string, unknown>
}

export interface RouteExplainResponse {
  host: string | null
  path: string
  method: string
  matches: Array<Record<string, unknown>>
  unmatched: string[]
}

export async function explainRoute(
  query: RouteExplainQuery,
  token: string,
  signal?: AbortSignal,
): Promise<RouteExplainResponse> {
  const params = new URLSearchParams({ path: query.path })
  if (query.host) params.set('host', query.host)
  if (query.method) params.set('method', query.method)
  if (query.listener) params.set('listener', query.listener)
  const value = await request(`/api/v1/routes/explain?${params.toString()}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.path !== 'string' || typeof value.method !== 'string' ||
    !Array.isArray(value.matches) || !Array.isArray(value.unmatched)
  ) return invalidPayload('route explain')
  return value as unknown as RouteExplainResponse
}

export async function fetchClients(token: string, signal?: AbortSignal): Promise<Record<string, unknown>> {
  const value = await request('/api/v1/clients', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value)) return invalidPayload('clients')
  return value
}

export async function fetchConnections(token: string, signal?: AbortSignal): Promise<Record<string, unknown>> {
  const value = await request('/api/v1/connections', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value)) return invalidPayload('connections')
  return value
}
