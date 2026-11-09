import type { Decoded, StatsSnapshot } from './decode'
import type {
  CertificateStats, CertificateWatcherStats, GenerationStats, ListenerStats, PoolStats, ProcessStats, RouteStats,
  ServerStats, ServerStatus, TrafficStats, WarningStats,
} from './generated/statsPage'
import type { HistogramCounts } from './percentiles'

// The view model: a decoded snapshot arranged as rows with stable keys. Rows keep the decoded wire object (`wire`),
// so nothing the server sent is lost, and add what every view needs: key, name, problem flag, status text, and the
// counters and histograms that rates and percentiles are derived from. Nothing is invented: a row has a counter or a
// histogram only when the server sent it.

/** Metrics with a rate (per second); see `rowCounters`. */
export const METRICS = ['requests', 'errors', 'bytesReceived', 'bytesSent', 'accepted'] as const
export type Metric = (typeof METRICS)[number]
export type HistogramName = 'request' | 'header' | 'wait'

export const CERT_SOON_MS = 14 * 86_400_000

export const STATUS_LABELS: Readonly<Record<ServerStatus, string>> = {
  up: 'UP', down: 'DOWN', drain: 'DRAIN', maint: 'MAINT', ejected: 'EJECTED', unchecked: 'UNCHECKED', unknown: 'UNKNOWN',
}

/** `kind:part/part`; parts are percent-escaped, so names containing `/`, `:` or `#` cannot collide. */
export const rowKey = (kind: string, ...parts: string[]) => `${kind}:${parts.map(encodeURIComponent).join('/')}`

export interface Row<Wire> {
  key: string
  name: string
  wire: Wire
  /** Needs attention: shown by the "problems only" filter and counted in the summary. */
  problem: boolean
}
export interface Metered {
  counters: Readonly<Partial<Record<Metric, bigint>>>
  histograms: Readonly<Partial<Record<HistogramName, HistogramCounts>>>
}
type Wire<T> = Decoded<T>

export type ListenerRow = Row<Wire<ListenerStats>> & Metered
export type ServerRow = Row<Wire<ServerStats>> & Metered & { pool: string; statusLabel: string }
export type PoolRow = Row<Wire<Omit<PoolStats, 'servers'>>> & Metered & { servers: ServerRow[]; statusLabel: string }
export type RouteRow = Row<Wire<RouteStats>>
export type WarningRow = Row<Wire<WarningStats>>
export type WatcherRow = Row<Wire<CertificateWatcherStats>>
export type ProcessRow = Row<Wire<ProcessStats>> & Metered
export type GenerationRow = Row<Wire<GenerationStats>>
export type CertificateRow = Row<Wire<CertificateStats>> & {
  /** Validity left at the server's sample time; negative once expired; absent when the expiry could not be read. */
  remainingMs?: number
  severity: 'ok' | 'soon' | 'expired' | 'unknown'
}

export interface Count { up: number; total: number }
export interface StatsModel {
  sampledAtUnixMs: number
  intervalHintMs: number
  /** The server offers administration to this client. */
  admin: boolean
  bucketBoundsMs: StatsSnapshot['bucketBoundsMs']
  process: ProcessRow
  generation: GenerationRow
  listeners: ListenerRow[]
  routes: RouteRow[]
  pools: PoolRow[]
  certificates: CertificateRow[]
  certificateWatchers: WatcherRow[]
  warnings: WarningRow[]
  summary: { listeners: Count; pools: Count; servers: Count; problems: number; warnings: number }
}

export function buildModel(snapshot: StatsSnapshot): StatsModel {
  const model = {
    sampledAtUnixMs: snapshot.sampledAtUnixMs,
    intervalHintMs: snapshot.intervalHintMs,
    admin: snapshot.admin,
    bucketBoundsMs: snapshot.bucketBoundsMs,
    process: processRow(snapshot.process),
    generation: generationRow(snapshot.generation),
    listeners: snapshot.listeners.map(listenerRow),
    routes: snapshot.routes.map((wire) => ({ key: rowKey('route', wire.id), name: wire.id, wire, problem: false })),
    pools: snapshot.pools.map(poolRow),
    certificates: snapshot.certificates.map((wire) => certificateRow(wire, snapshot.sampledAtUnixMs)),
    certificateWatchers: snapshot.certificateWatchers.map(watcherRow),
    warnings: warningRows(snapshot.warnings),
  }
  return { ...model, summary: summarize(model) }
}

/** Every counter of the model by `<row key>#<metric>`, the input of the rate tracker. */
export function modelCounters(model: StatsModel): Map<string, bigint> {
  const counters = new Map<string, bigint>()
  const rows: Array<Metered & { key: string }> = [
    model.process, ...model.listeners, ...model.pools, ...model.pools.flatMap((pool) => pool.servers),
  ]
  for (const row of rows) {
    for (const [metric, value] of Object.entries(row.counters)) counters.set(`${row.key}#${metric}`, value)
  }
  return counters
}

const FAILING_OUTCOMES = new Set(['server_error', 'upstream_error', 'timeout', 'internal_error'])

const defined = <T extends object>(source: { [K in keyof T]: T[K] | undefined }): T =>
  Object.fromEntries(Object.entries(source).filter(([, value]) => value !== undefined)) as T

function processRow(wire: Wire<ProcessStats>): ProcessRow {
  return {
    key: 'process', name: 'process', wire, problem: wire.state === 'degraded' || wire.host.state === 'degraded',
    counters: { accepted: wire.acceptedConnections, bytesReceived: wire.bytesReceived, bytesSent: wire.bytesSent },
    histograms: {},
  }
}

function generationRow(wire: Wire<GenerationStats>): GenerationRow {
  return { key: 'generation', name: `generation ${wire.number}`, wire, problem: wire.degraded || !wire.accepting }
}

function listenerRow(wire: Wire<ListenerStats>): ListenerRow {
  const outcomes = wire.http?.outcomes
  const failing = outcomes?.filter((outcome) => FAILING_OUTCOMES.has(outcome.result)).reduce((sum, { count }) => sum + count, 0n)
  return {
    key: rowKey('listener', wire.name), name: wire.name, wire, problem: wire.state !== 'listening',
    counters: defined({
      accepted: wire.acceptedConnections, bytesReceived: wire.bytesReceived, bytesSent: wire.bytesSent,
      requests: wire.http?.total ?? wire.tcpRelays?.total, errors: failing,
    }),
    histograms: defined({ request: wire.http?.latency ?? wire.tcpRelays?.latency }),
  }
}

/** Failed attempts (connect failures, timeouts, protocol errors) plus 5xx answers. */
const trafficErrors = ({ attempts, responses }: Wire<TrafficStats>) =>
  attempts.connectFailed + attempts.timeout + attempts.protocolError + responses['5xx']

const trafficCounters = (traffic: Wire<TrafficStats>): Metered['counters'] => ({
  requests: traffic.sessions.total, errors: trafficErrors(traffic),
  bytesReceived: traffic.bytesReceived, bytesSent: traffic.bytesSent,
})

function serverRow(pool: string, wire: Wire<ServerStats>): ServerRow {
  return {
    key: rowKey('server', pool, wire.name), name: wire.name, pool, wire, statusLabel: STATUS_LABELS[wire.status],
    problem: wire.status !== 'up' && wire.status !== 'unchecked',
    counters: trafficCounters(wire.traffic),
    histograms: { request: wire.latency.request, header: wire.latency.header },
  }
}

function poolRow({ servers: serverWire, ...wire }: Wire<PoolStats>): PoolRow {
  const servers = serverWire.map((server) => serverRow(wire.name, server))
  return {
    key: rowKey('pool', wire.name), name: wire.name, wire, servers, statusLabel: wire.status.toUpperCase(),
    problem: wire.status !== 'up' || servers.some((server) => server.problem),
    counters: trafficCounters(wire.traffic),
    histograms: { request: wire.latency.request, header: wire.latency.header, wait: wire.queue.wait },
  }
}

function certificateRow(wire: Wire<CertificateStats>, sampledAtUnixMs: number): CertificateRow {
  const remainingMs = wire.notAfterUnixSeconds === undefined ? undefined : wire.notAfterUnixSeconds * 1000 - sampledAtUnixMs
  const severity = remainingMs === undefined ? 'unknown' : remainingMs < 0 ? 'expired' : remainingMs < CERT_SOON_MS ? 'soon' : 'ok'
  return {
    key: rowKey('certificate', wire.name), name: wire.name, wire, severity, problem: severity === 'expired' || severity === 'soon',
    ...(remainingMs === undefined ? {} : { remainingMs }),
  }
}

const watcherRow = (wire: Wire<CertificateWatcherStats>): WatcherRow =>
  ({ key: rowKey('watcher', wire.source), name: wire.source, wire, problem: wire.health !== 'healthy' })

function warningRows(warnings: Wire<WarningStats>[]): WarningRow[] {
  const seen = new Map<string, number>()
  return warnings.map((wire) => {
    const occurrence = (seen.get(wire.code) ?? 0) + 1
    seen.set(wire.code, occurrence)
    const key = rowKey('warning', wire.code) + (occurrence > 1 ? `#${occurrence}` : '')
    return { key, name: wire.code, wire, problem: false }
  })
}

function summarize(model: Omit<StatsModel, 'summary'>): StatsModel['summary'] {
  const servers = model.pools.flatMap((pool) => pool.servers)
  const problems = [model.process, model.generation, ...model.listeners, ...model.pools, ...servers, ...model.certificates,
    ...model.certificateWatchers]
  return {
    listeners: { up: model.listeners.filter((row) => !row.problem).length, total: model.listeners.length },
    pools: { up: model.pools.filter((pool) => pool.wire.status === 'up').length, total: model.pools.length },
    servers: { up: servers.filter((server) => server.wire.status === 'up').length, total: servers.length },
    problems: problems.filter((row) => row.problem).length,
    warnings: model.warnings.length,
  }
}
