import type { Change, Json } from './tree'

const VALUE_LIMIT = 60

export function formatValue(value: Json): string {
  if (value === undefined) return 'absent'
  const text = JSON.stringify(value)
  return text.length > VALUE_LIMIT ? `${text.slice(0, VALUE_LIMIT - 1)}…` : text
}

export function pathSummary(changes: Change[], limit = 6): string[] {
  const shown = changes.slice(0, limit).map(({ path }) => path)
  return changes.length > limit ? [...shown, `and ${changes.length - limit} more`] : shown
}
