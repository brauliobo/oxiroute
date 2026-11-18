import type { Tone } from '#design/badge/tone'
import { shortRevision } from '../formatters'
import type { OperationalEvacuationOutcome, OperationalEvent, OperationalEventOutcome } from '../api'

const OUTCOME_TONES: Readonly<Record<string, Tone>> = {
  applied: 'ok',
  activated: 'ok',
  drained: 'ok',
  recovered: 'ok',
  prepared: 'info',
  requested: 'info',
  retiring: 'info',
  health_changed: 'info',
  evacuated: 'info',
  forced_close: 'warn',
  cap_reached: 'warn',
  rejected: 'bad',
  failed: 'bad',
  ejected: 'bad',
  quarantined: 'bad',
  unknown: 'neutral',
}

export const sentence = (name: string): string => {
  const words = name.replaceAll('_', ' ')
  return `${words.slice(0, 1).toUpperCase()}${words.slice(1)}`
}

export const outcomeType = (outcome: OperationalEventOutcome): string =>
  typeof outcome === 'string' ? outcome : outcome.type

export const outcomeTone = (outcome: OperationalEventOutcome): Tone => OUTCOME_TONES[outcomeType(outcome)] ?? 'neutral'

export const outcomeLabel = (outcome: OperationalEventOutcome): string => sentence(outcomeType(outcome))

function evacuationDetail(outcome: OperationalEvacuationOutcome): string {
  const target = outcome.server === undefined ? outcome.pool : `${outcome.pool} / ${outcome.server}`
  const left = Object.values(outcome.leftRunning).reduce((total, count) => total + count, 0)
  return `${target} / ${outcome.cancelledAndRedispatched} moved, ${outcome.forced} forced, ${left} left running`
}

export function outcomeDetail(outcome: OperationalEventOutcome): string | null {
  if (typeof outcome === 'string') return null
  if (outcome.type === 'evacuated') return evacuationDetail(outcome)
  const reason = outcome.reason === null ? '' : ` / ${outcome.reason.replaceAll('_', ' ')}`
  return `${outcome.pool} / ${outcome.server}${reason}`
}

export function eventDetails(event: OperationalEvent): string[] {
  const detail = outcomeDetail(event.outcome)
  return [event.certificate && `Certificate ${event.certificate}`, detail].filter((part): part is string => Boolean(part))
}

export const eventNotes = (event: OperationalEvent): string[] => [
  ...eventDetails(event),
  ...(event.revision ? [`Revision ${shortRevision(event.revision)}`] : []),
]
