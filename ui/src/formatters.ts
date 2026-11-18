import { formatAge } from '#design/format'

export { formatBytes, formatDuration as formatTelemetryDuration, formatExact as formatCount } from '#design/format'

const dateTimeFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'short',
  timeStyle: 'medium',
})
const clockTimeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
})

export function formatTelemetryAge(timestamp: number, now = Date.now()): string {
  return formatAge(Math.max(0, now - timestamp))
}

export function formatTime(timestamp: number): string {
  return dateTimeFormatter.format(timestamp)
}

export function formatClockTime(timestamp: number): string {
  return clockTimeFormatter.format(timestamp)
}

export function shortRevision(revision: string | null): string {
  if (!revision) return 'None'
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

export function presentApiError(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback
}
