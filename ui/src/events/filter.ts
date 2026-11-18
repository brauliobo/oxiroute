import type { SelectOption } from '#design/field/types'
import type { OperationalEvent } from '../api'
import { eventNotes, outcomeLabel, sentence } from './format'

export const ALL_EVENTS = ''

const searchable = (event: OperationalEvent): string =>
  [event.cursor, event.event, sentence(event.event), outcomeLabel(event.outcome), ...eventNotes(event)].join(' ').toLowerCase()

export function filterEvents(events: readonly OperationalEvent[], text: string, name: string): OperationalEvent[] {
  const needle = text.trim().toLowerCase()
  return events.filter((event) =>
    (name === ALL_EVENTS || event.event === name) && (needle === '' || searchable(event).includes(needle)))
}

export function eventNameOptions(events: readonly OperationalEvent[]): SelectOption[] {
  const names = [...new Set(events.map(({ event }) => event))].sort()
  return [{ value: ALL_EVENTS, label: 'All events' }, ...names.map((value) => ({ value, label: sentence(value) }))]
}

export const ringRestarted = (seenLatestCursor: number, reportedLatestCursor: number): boolean =>
  reportedLatestCursor < seenLatestCursor
