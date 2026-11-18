import type { MonitoringSnapshot } from '../api'
import { formatBytes, formatCount, formatTelemetryAge } from '../formatters'

type Host = MonitoringSnapshot['host']

const UNAVAILABLE = 'Unavailable'

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

export const formatOptionalBytes = (value: number | null): string => (value === null ? UNAVAILABLE : formatBytes(value))

export const formatOptionalCount = (value: number | null): string => (value === null ? UNAVAILABLE : formatCount(value))

export const formatLimit = (value: number | null): string => (value === null ? 'Unbounded' : formatCount(value))

export const formatDecimal = (value: number | null): string => (value === null ? UNAVAILABLE : value.toFixed(2))

export function formatOptionalPercent(value: number | null): string {
  return value === null ? UNAVAILABLE : `${value.toFixed(1).replace(/\.0$/, '')}%`
}

export const formatSampleAge = (currentUnixMs: number, sampledAt: number): string =>
  formatTelemetryAge(sampledAt, currentUnixMs)
