import { arrayOf, isRecord, nullableString } from '../../valueGuards'
import { request } from '../transport'
import { authorizationHeader, invalidPayload, mutationHeaders } from './shared'

export type OverlayState = 'empty' | 'applied' | 'diverged' | 'unreadable'

export interface OverlaySetting {
  target: string
  setting: string
  value: string
  status: string
}

export interface OverlaySnapshot {
  path: string
  revision: string | null
  state: OverlayState
  settings: OverlaySetting[]
}

export interface OverlayPromoteResponse {
  outcome: string
  promoted?: number
  diskRevision?: string
  candidateRevision?: string
}

export async function fetchOverlay(token: string, signal?: AbortSignal): Promise<OverlaySnapshot> {
  const value = await request('/api/v1/overlay', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.path !== 'string' || !nullableString(value.revision) ||
    !['empty', 'applied', 'diverged', 'unreadable'].includes(String(value.state)) ||
    !arrayOf(value.settings, isOverlaySetting)
  ) return invalidPayload('overlay')
  return {
    path: value.path,
    revision: value.revision,
    state: value.state as OverlayState,
    settings: value.settings,
  }
}

export async function clearOverlay(token: string, signal?: AbortSignal): Promise<{ outcome: string; changed?: number }> {
  const value = await request('/api/v1/overlay/clear', {
    method: 'POST',
    headers: mutationHeaders(token),
    body: '{}',
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.outcome !== 'string') return invalidPayload('overlay clear')
  return value as { outcome: string; changed?: number }
}

export async function promoteOverlay(
  diskRevision: string,
  token: string,
  signal?: AbortSignal,
): Promise<OverlayPromoteResponse> {
  const value = await request('/api/v1/config/promote-overlay', {
    method: 'POST',
    headers: {
      ...mutationHeaders(token),
      'If-Config-Revision': diskRevision,
    },
    body: '{}',
    signal,
  }, 200)
  if (!isRecord(value) || typeof value.outcome !== 'string') return invalidPayload('overlay promote')
  return value as unknown as OverlayPromoteResponse
}

function isOverlaySetting(value: unknown): value is OverlaySetting {
  return isRecord(value) && typeof value.target === 'string' && typeof value.setting === 'string' &&
    typeof value.value === 'string' && typeof value.status === 'string'
}
