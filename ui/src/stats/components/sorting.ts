import type { SortColumn } from '../query'
import type { SortSpec } from '../urlState'

// Clicking a sortable header cycles first direction, the other direction, then back to the source order. Names and
// statuses start ascending (A to Z, worst first); the numbers start descending (largest first).

const ASCENDING_FIRST: readonly SortColumn[] = ['name', 'status']

export function nextSort(current: SortSpec | undefined, column: SortColumn): SortSpec | undefined {
  const first = ASCENDING_FIRST.includes(column) ? 'asc' : 'desc'
  if (current?.column !== column) return { column, direction: first }
  return current.direction === first ? { column, direction: first === 'asc' ? 'desc' : 'asc' } : undefined
}

export const ariaSort = (current: SortSpec | undefined, column: SortColumn) =>
  current?.column !== column ? 'none' : current.direction === 'asc' ? 'ascending' : 'descending'
