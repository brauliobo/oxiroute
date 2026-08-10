const numberFormatter = new Intl.NumberFormat()
const dateTimeFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'short',
  timeStyle: 'medium',
})
const clockTimeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
})

export function formatCount(value: number | string): string {
  return numberFormatter.format(typeof value === 'string' ? BigInt(value) : value)
}

export function formatBytes(value: number | string): string {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const amount = bytes / 1024 ** exponent
  return `${amount >= 10 || exponent === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[exponent]}`
}

export function formatTelemetryDuration(durationMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(durationMs / 1000))
  const days = Math.floor(totalSeconds / 86_400)
  const hours = Math.floor((totalSeconds % 86_400) / 3_600)
  const minutes = Math.floor((totalSeconds % 3_600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m ${totalSeconds % 60}s`
  return `${totalSeconds}s`
}

export function formatTelemetryAge(timestamp: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000))
  if (seconds < 2) return 'just now'
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.floor(seconds / 60)
  return minutes < 60 ? `${minutes}m ago` : `${Math.floor(minutes / 60)}h ago`
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
