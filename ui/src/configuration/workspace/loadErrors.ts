import { ApiError } from '../../api'
import { errorDiagnosticsFrom } from '../../config'
import type { ConfigDiagnostic } from '../../config'
import { isRecord } from '../../valueGuards'

export interface CanonicalUnavailableState {
  diskRevision: string | null
  activeRevision: string | null
  diagnostics: ConfigDiagnostic[]
}

export function configRouteUnavailable(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 404) return false
  return error.code === null || error.code === 'route_not_found'
}

export function canonicalUnavailablePayload(error: unknown): CanonicalUnavailableState | null {
  if (!(error instanceof ApiError) || error.status !== 503 ||
    error.code !== 'canonical_config_unavailable' || !isRecord(error.payload)
  ) return null
  const payload = error.payload
  return {
    diskRevision: typeof payload.diskRevision === 'string' ? payload.diskRevision : null,
    activeRevision: typeof payload.activeRevision === 'string' ? payload.activeRevision : null,
    diagnostics: errorDiagnosticsFrom(payload),
  }
}
