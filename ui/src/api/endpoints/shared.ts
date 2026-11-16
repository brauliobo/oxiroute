import type { ConfigDiagnostic } from '../../config'
import { isConfigDiagnostic } from '../../config'
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

export function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
