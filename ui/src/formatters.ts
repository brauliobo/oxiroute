import { formatAge, formatExact, formatTimestamp } from '#design/format'

export {
  formatBytes, formatDuration as formatTelemetryDuration, formatExact as formatCount, formatTimestamp as formatTime,
} from '#design/format'

export const NO_DATA_YET = 'no data yet'

export function formatTelemetryAge(timestamp: number, now = Date.now()): string {
  return formatAge(Math.max(0, now - timestamp))
}

export function formatSnapshotTime(unixMs: number): string {
  return unixMs === 0 ? NO_DATA_YET : formatTimestamp(unixMs)
}

export function pluralize(count: number | string, singular: string, plural = `${singular}s`): string {
  return `${formatExact(count)} ${String(count) === '1' ? singular : plural}`
}

export function shortRevision(revision: string | null): string {
  if (!revision) return 'None'
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

export function presentApiError(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback
}
