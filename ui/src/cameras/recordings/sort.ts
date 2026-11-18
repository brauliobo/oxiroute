import type { SelectOption } from '#design/field/types'
import type { Recording } from '../../api'
import { recordedAt } from './filters'
import { STATUS_RANK } from './status'

export const SORT_KEYS = ['name', 'stream', 'started', 'duration', 'size', 'status'] as const
export type SortKey = (typeof SORT_KEYS)[number]
export type SortDirection = 'ascending' | 'descending'

export interface SortState {
  key: SortKey
  direction: SortDirection
}

export const SORT_LABELS: Readonly<Record<SortKey, string>> = {
  name: 'Name',
  stream: 'Stream',
  started: 'Started',
  duration: 'Duration',
  size: 'Size',
  status: 'Status',
}

const FIRST_DIRECTION: Readonly<Record<SortKey, SortDirection>> = {
  name: 'ascending',
  stream: 'ascending',
  started: 'descending',
  duration: 'descending',
  size: 'descending',
  status: 'ascending',
}

export const DEFAULT_SORT: SortState = { key: 'started', direction: 'descending' }

export function nextSort(current: SortState, key: SortKey): SortState {
  if (current.key !== key) return { key, direction: FIRST_DIRECTION[key] }
  return { key, direction: current.direction === 'ascending' ? 'descending' : 'ascending' }
}

export const ariaSort = (current: SortState, key: SortKey): SortDirection | 'none' =>
  current.key === key ? current.direction : 'none'

const order = (left: number | bigint | string, right: number | bigint | string): number =>
  left < right ? -1 : left > right ? 1 : 0

function nullable(left: number | string | null, right: number | string | null, sign: number): number {
  if (left === null || right === null) return left === right ? 0 : left === null ? 1 : -1
  return sign * order(left, right)
}

function compareBy(key: SortKey, left: Recording, right: Recording, sign: number): number {
  switch (key) {
    case 'name': return sign * order(left.name, right.name)
    case 'stream': return nullable(left.stream, right.stream, sign)
    case 'started': return sign * order(recordedAt(left), recordedAt(right))
    case 'duration': return nullable(left.duration_ms, right.duration_ms, sign)
    case 'size': return sign * order(BigInt(left.size_bytes), BigInt(right.size_bytes))
    case 'status': return sign * order(STATUS_RANK[left.status], STATUS_RANK[right.status])
  }
}

export function sortRecordings(recordings: readonly Recording[], sort: SortState): Recording[] {
  const sign = sort.direction === 'ascending' ? 1 : -1
  return [...recordings].sort((left, right) =>
    compareBy(sort.key, left, right, sign) || order(recordedAt(right), recordedAt(left)) || order(left.id, right.id))
}

const CHOICES = new Map<string, SortState>(
  SORT_KEYS.flatMap((key) => (['ascending', 'descending'] as const).map((direction) => [`${key}:${direction}`, { key, direction }])),
)

export const sortValue = (sort: SortState): string => `${sort.key}:${sort.direction}`

export const parseSort = (value: string): SortState | undefined => CHOICES.get(value)

export const SORT_OPTIONS: readonly SelectOption[] = [...CHOICES].map(([value, { key, direction }]) => ({
  value,
  label: `${SORT_LABELS[key]} (${direction})`,
}))
