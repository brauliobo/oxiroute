import { arrayOf, isRecord, isString, nullableSafeInteger, nullableString, safeInteger } from '../../valueGuards'
import { ApiError, request } from '../transport'
import type { AdministrativeState } from '../managementContracts'
import { authorizationHeader, invalidPayload, mutationHeaders } from './shared'

export type RegistrationStatus = 'active' | 'shadowed' | 'orphaned'
export type AnnouncedState = 'ready' | 'draining'

export interface Registration {
  pool: string
  name: string
  address: string
  weight: number
  priority: number
  maxConnections: number | null
  labels: Record<string, string>
  metadata: unknown
  ttlMs: number
  announcedState: AnnouncedState
  operatorState: AdministrativeState
  effectiveState: AdministrativeState
  registeredBy: string
}

export interface RegistrationEntry {
  registration: Registration
  expiresInMs: number | null
  status: RegistrationStatus
}

export interface RegistrationList {
  registrations: RegistrationEntry[]
  registryRevision: string
  writer: boolean
  degraded: string | null
}

const STATUSES: readonly string[] = ['active', 'shadowed', 'orphaned']
const ANNOUNCED_STATES: readonly string[] = ['ready', 'draining']
const ADMINISTRATIVE_STATES: readonly string[] = ['ready', 'drain', 'maintenance']

export async function fetchRegistrations(token: string, signal?: AbortSignal): Promise<RegistrationList> {
  return parseRegistrationList(await request('/api/v1/registrations', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function setRegisteredServerState(
  pool: string,
  name: string,
  state: AdministrativeState,
  token: string,
): Promise<void> {
  await request(`/api/v1/pools/${pool}/servers/${name}/state`, {
    method: 'PUT',
    headers: mutationHeaders(token),
    body: JSON.stringify({ state }),
  })
}

export async function deregisterPoolServer(pool: string, name: string, token: string): Promise<void> {
  await request(`/api/v1/pools/${pool}/servers/${name}`, {
    method: 'DELETE',
    headers: authorizationHeader(token),
  })
}

export function registryDisabled(error: unknown): boolean {
  return error instanceof ApiError && error.code === 'registry_disabled'
}

function oneOf(value: unknown, values: readonly string[]): boolean {
  return isString(value) && values.includes(value)
}

function labels(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every(isString)
}

function registration(value: unknown): value is Registration {
  return isRecord(value) && isString(value.pool) && isString(value.name) && isString(value.address) &&
    safeInteger(value.weight) && safeInteger(value.priority) && nullableSafeInteger(value.maxConnections) &&
    labels(value.labels) && value.metadata !== undefined && safeInteger(value.ttlMs) &&
    oneOf(value.announcedState, ANNOUNCED_STATES) && oneOf(value.operatorState, ADMINISTRATIVE_STATES) &&
    oneOf(value.effectiveState, ADMINISTRATIVE_STATES) && isString(value.registeredBy)
}

function registrationEntry(value: unknown): value is RegistrationEntry {
  return isRecord(value) && registration(value.registration) && nullableSafeInteger(value.expiresInMs) &&
    oneOf(value.status, STATUSES)
}

function parseRegistrationList(value: unknown): RegistrationList {
  if (!isRecord(value) || !arrayOf(value.registrations, registrationEntry) || !isString(value.registryRevision) ||
    typeof value.writer !== 'boolean' || !nullableString(value.degraded)
  ) return invalidPayload('registrations')
  return {
    registrations: value.registrations,
    registryRevision: value.registryRevision,
    writer: value.writer,
    degraded: value.degraded,
  }
}
