import type { ConfigDiagnostic, StartupOnlyChange } from '../../config'
import { isConfigDiagnostic, isStartupOnlyChange } from '../../config'
import { isRecord, safeInteger } from '../../valueGuards'

export function isCountMap(value: unknown): value is Record<string, number> {
  return isRecord(value) && Object.values(value).every(safeInteger)
}

export function mutationHeaders(token: string): Record<string, string> {
  return { ...authorizationHeader(token), 'Content-Type': 'application/json' }
}

export function authorizationHeader(token: string): Record<string, string> {
  return { Authorization: `Bearer ${token}` }
}

export function diagnostics(value: unknown): value is ConfigDiagnostic[] {
  return Array.isArray(value) && value.every(isConfigDiagnostic)
}

export function restartReasons(value: unknown): value is StartupOnlyChange[] {
  return Array.isArray(value) && value.every(isStartupOnlyChange)
}

export function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
