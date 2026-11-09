import type { MeasuredModel } from '../derive'
import type { CertificateRow, Count } from '../model'

export interface Overview {
  version: string
  uptimeMs: number
  generation: { number: number; accepting: boolean; degraded: boolean; activeRevision: string | undefined }
  connections: { active: number; max: number | undefined; accepted: bigint }
  /** Summed over the listeners whose rate is known; absent while none is. */
  requestRate: number | undefined
  bytesInRate: number | undefined
  bytesOutRate: number | undefined
  /** Failing share of requests, in percent; absent without traffic. */
  errorPercent: number | undefined
  pools: Count & { degraded: number; down: number; empty: number }
  servers: Count
  listeners: Count
  /** The certificate that expires first (or has expired longest ago). */
  certificate: { name: string; remainingMs: number; severity: CertificateRow['severity'] } | undefined
  warnings: number
}

const sum = (values: Array<number | undefined>) => {
  const known = values.filter((value): value is number => value !== undefined)
  return known.length === 0 ? undefined : known.reduce((total, value) => total + value, 0)
}

export function summarizeOverview(model: MeasuredModel): Overview {
  const { process, generation, listeners, pools, certificates, summary } = model
  const requestRate = sum(listeners.map((listener) => listener.rates.requests))
  const errorRate = sum(listeners.map((listener) => listener.rates.errors))
  const soonest = certificates.flatMap((row) => (row.remainingMs === undefined ? [] : [{ ...row, remainingMs: row.remainingMs }]))
    .sort((left, right) => left.remainingMs - right.remainingMs)[0]
  const poolsWith = (status: string) => pools.filter((pool) => pool.wire.status === status).length
  return {
    version: process.wire.version,
    uptimeMs: process.wire.uptimeMs,
    generation: {
      number: generation.wire.number, accepting: generation.wire.accepting, degraded: generation.wire.degraded,
      activeRevision: generation.wire.activeRevision,
    },
    connections: { active: process.active, max: process.wire.maxConnections, accepted: process.wire.acceptedConnections },
    requestRate,
    bytesInRate: process.rates.bytesReceived,
    bytesOutRate: process.rates.bytesSent,
    errorPercent: requestRate && errorRate !== undefined ? (errorRate / requestRate) * 100 : undefined,
    pools: { ...summary.pools, degraded: poolsWith('degraded'), down: poolsWith('down'), empty: poolsWith('empty') },
    servers: summary.servers,
    listeners: summary.listeners,
    certificate: soonest && { name: soonest.name, remainingMs: soonest.remainingMs, severity: soonest.severity },
    warnings: summary.warnings,
  }
}
