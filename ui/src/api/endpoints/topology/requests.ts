import { request } from '../../transport'
import { authorizationHeader } from '../shared'
import { parseTopology } from './decode'
import type { TopologySnapshot } from './types'

export async function fetchTopology(token: string, signal?: AbortSignal): Promise<TopologySnapshot> {
  return parseTopology(await request('/api/v1/topology', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}
