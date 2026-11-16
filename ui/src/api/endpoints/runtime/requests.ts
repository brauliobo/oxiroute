import { parseListenerInventory, parsePoolInventory, parseRuntimeStatus, parseServerInventory } from '../../decoders'
import { request } from '../../transport'
import type {
  ListenerInventoryResponse,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryResponse,
} from '../../managementContracts'
import { authorizationHeader } from '../shared'
import { parseGenerationResponse } from './decode'
import type { GenerationResponse } from './types'

export async function fetchStatus(token: string, signal?: AbortSignal): Promise<RuntimeStatus> {
  return parseRuntimeStatus(await request('/api/v1/status', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchListeners(token: string, signal?: AbortSignal): Promise<ListenerInventoryResponse> {
  return parseListenerInventory(await request('/api/v1/listeners', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchPools(token: string, signal?: AbortSignal): Promise<PoolInventoryResponse> {
  return parsePoolInventory(await request('/api/v1/pools', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchServers(token: string, signal?: AbortSignal): Promise<ServerInventoryResponse> {
  return parseServerInventory(await request('/api/v1/servers', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchGenerations(token: string, signal?: AbortSignal): Promise<GenerationResponse> {
  return parseGenerationResponse(await request('/api/v1/generations', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}
