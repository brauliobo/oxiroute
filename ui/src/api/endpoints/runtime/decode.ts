import { isRecord, nullableString, safeInteger } from '../../../valueGuards'
import { invalidPayload } from '../shared'
import type { GenerationResponse, GenerationStatus } from './types'

export function parseGenerationResponse(value: unknown): GenerationResponse {
  if (!isRecord(value) || !generationStatus(value.generation)) return invalidPayload('generation status')
  return { generation: value.generation }
}

function nullableRevision(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

function generationStatus(value: unknown): value is GenerationStatus {
  return isRecord(value) && typeof value.buildVersion === 'string' &&
    nullableRevision(value.diskRevision) && nullableRevision(value.candidateRevision) &&
    nullableRevision(value.activeRevision) && nullableRevision(value.previousRevision) &&
    nullableRevision(value.quarantinedRevision) && typeof value.activeAccepting === 'boolean' &&
    typeof value.degraded === 'boolean' && nullableString(value.lastFailure) &&
    safeInteger(value.prepares) && safeInteger(value.activations) && safeInteger(value.failures) &&
    safeInteger(value.rollbacks)
}
