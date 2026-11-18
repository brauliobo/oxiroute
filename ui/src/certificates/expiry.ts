import type { Tone } from '#design/badge/tone'
import { plural } from '../workspace/plural'

const HOUR_MS = 3_600_000
const DAY_MS = 24 * HOUR_MS
export const EXPIRING_SOON_DAYS = 30

export interface ExpiryBadge {
  tone: Tone
  label: string
}

function span(milliseconds: number): string {
  if (milliseconds >= DAY_MS) return plural(Math.floor(milliseconds / DAY_MS), 'day')
  if (milliseconds >= HOUR_MS) return plural(Math.floor(milliseconds / HOUR_MS), 'hour')
  return 'less than an hour'
}

export function expiryPhrase(expiresAtMs: number, nowMs: number): string {
  const remaining = expiresAtMs - nowMs
  return remaining >= 0 ? `expires in ${span(remaining)}` : `expired ${span(-remaining)} ago`
}

export function expiryBadge(expiresAtMs: number, nowMs: number): ExpiryBadge | null {
  const remaining = expiresAtMs - nowMs
  if (remaining < 0) return { tone: 'bad', label: 'Expired' }
  if (remaining <= EXPIRING_SOON_DAYS * DAY_MS) return { tone: 'warn', label: 'Expires soon' }
  return null
}
