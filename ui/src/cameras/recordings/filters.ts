import type { SelectOption } from '#design/field/types'
import type { Recording, RecordingFilter } from '../../api'
import { plural } from '../../workspace/plural'
import { asStatus, STATUSES } from './status'

export interface Filters {
  stream: string
  status: string
  from: string
  to: string
  codec: string
  search: string
}

export type FilterName = keyof Filters

export const PAGE_SIZE = 50
const DAY_MS = 86_400_000

export const emptyFilters = (): Filters => ({ stream: '', status: '', from: '', to: '', codec: '', search: '' })

export const isServerFilter = (name: FilterName): boolean => name === 'stream' || name === 'status'

export const hasFilter = (filters: Filters): boolean => Object.values(filters).some((value) => value.trim() !== '')

export const serverQuery = (filters: Filters, cursor?: string): RecordingFilter => ({
  stream: filters.stream,
  status: asStatus(filters.status),
  limit: PAGE_SIZE,
  cursor,
})

export const recordedAt = (recording: Recording): number => recording.started_at_unix_ms ?? recording.modified_at_unix_ms

const dayStart = (day: string): number => Date.parse(`${day}T00:00:00Z`)

export function matchesFilters(recording: Recording, filters: Filters): boolean {
  const at = recordedAt(recording)
  const needle = filters.search.trim().toLowerCase()
  return (filters.stream === '' || recording.stream === filters.stream) &&
    (filters.status === '' || recording.status === filters.status) &&
    (filters.from === '' || at >= dayStart(filters.from)) &&
    (filters.to === '' || at < dayStart(filters.to) + DAY_MS) &&
    (filters.codec === '' || recording.video_codec === filters.codec) &&
    (needle === '' || recording.name.toLowerCase().includes(needle))
}

export function mergeSeen(seen: readonly string[], found: readonly (string | null)[]): readonly string[] {
  const next = new Set(seen)
  for (const value of found) if (value !== null && value !== '') next.add(value)
  return next.size === seen.length ? seen : [...next].sort()
}

export function valueOptions(allLabel: string, seen: readonly string[], selected: string): SelectOption[] {
  const values = selected === '' || seen.includes(selected) ? seen : [...seen, selected].sort()
  return [{ value: '', label: allLabel }, ...values.map((value) => ({ value, label: value }))]
}

export const STATUS_OPTIONS: readonly SelectOption[] = [
  { value: '', label: 'All statuses' },
  ...STATUSES.map((status) => ({ value: status, label: `${status[0]!.toUpperCase()}${status.slice(1)}` })),
]

export function listSummary(shown: number, loaded: number, more: boolean): string {
  const noun = more ? 'loaded recordings' : 'recordings'
  return `Showing ${shown} of ${loaded} ${noun}${more ? '; more are available' : ''}.`
}

export const sortNote = (loaded: number): string => `Sorted among the ${plural(loaded, 'loaded recording')}.`
