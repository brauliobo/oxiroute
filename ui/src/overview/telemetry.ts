import type { MonitoringSnapshot } from '../api'
import { formatBytes, formatCount } from '../formatters'

type Host = MonitoringSnapshot['host']

export function totalTrafficBytes({ bytesReceived, bytesSent }: MonitoringSnapshot['traffic']): string {
  return (BigInt(bytesReceived) + BigInt(bytesSent)).toString()
}

export function usedMemoryBytes({ totalMemoryBytes, availableMemoryBytes }: Host): number | null {
  return totalMemoryBytes === null || availableMemoryBytes === null
    ? null
    : Math.max(0, totalMemoryBytes - availableMemoryBytes)
}

export function memoryUsagePercent(host: Host): number | null {
  const total = host.totalMemoryBytes
  const used = usedMemoryBytes(host)
  if (total === null || used === null) return null
  return total === 0 ? 0 : Math.min(100, Math.max(0, (used / total) * 100))
}

export function formatOptionalBytes(value: number | null): string {
  return value === null ? 'Unavailable' : formatBytes(value)
}

export function formatOptionalCount(value: number | null): string {
  return value === null ? 'Unavailable' : formatCount(value)
}

export function formatLimit(value: number | null): string {
  return value === null ? 'Unbounded' : formatCount(value)
}

export function formatDecimal(value: number | null): string {
  return value === null ? 'Unavailable' : value.toFixed(2)
}

export function formatOptionalPercent(value: number | null): string {
  return value === null ? 'Unavailable' : `${value.toFixed(1).replace(/\.0$/, '')}%`
}

export function formatSampleAge(currentUnixMs: number, sampledAt: number): string {
  const seconds = Math.max(0, Math.floor((currentUnixMs - sampledAt) / 1000))
  if (seconds < 2) return 'just now'
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.floor(seconds / 60)
  return minutes < 60 ? `${minutes}m ago` : `${Math.floor(minutes / 60)}h ago`
}
