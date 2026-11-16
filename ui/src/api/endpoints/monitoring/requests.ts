import { request } from '../../transport'
import { authorizationHeader } from '../shared'
import { parseMonitoring } from './decode'
import type { MonitoringSnapshot } from './types'

export async function fetchMonitoring(signal?: AbortSignal, token?: string): Promise<MonitoringSnapshot> {
  return parseMonitoring(await request('/api/v1/monitoring', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}
