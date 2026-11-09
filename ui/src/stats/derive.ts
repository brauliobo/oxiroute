import type { HistogramName, Metered, Metric, PoolRow, ProcessRow, ListenerRow, ServerRow, StatsModel } from './model'
import { summarizeLatency } from './percentiles'
import type { LatencySummary } from './percentiles'

// Rates and percentiles are not part of a snapshot: the first needs the previous snapshot, the second a reading of the
// histogram. They are attached to the rows of the model here, always as "known or absent".

export interface Measured {
  /** Per second, from counter deltas; absent while unknown (first sample, restart, gap). */
  rates: Partial<Record<Metric, number>>
  /** Lifetime latency percentiles; absent for a histogram without observations. */
  latency: Partial<Record<HistogramName, LatencySummary>>
}
export type MeasuredRow<R> = R & Measured
export type MeasuredPool = Omit<MeasuredRow<PoolRow>, 'servers'> & { servers: MeasuredRow<ServerRow>[] }
export type MeasuredModel = Omit<StatsModel, 'process' | 'listeners' | 'pools'> & {
  process: MeasuredRow<ProcessRow>
  listeners: MeasuredRow<ListenerRow>[]
  pools: MeasuredPool[]
}

export function deriveModel(model: StatsModel, rates: ReadonlyMap<string, number>): MeasuredModel {
  const operation = model.bucketBoundsMs.operation
  const queueWait = model.bucketBoundsMs.queueWait
  const measure = <R extends Metered & { key: string }>(row: R): MeasuredRow<R> => ({
    ...row,
    rates: Object.fromEntries(Object.keys(row.counters).flatMap((metric) => {
      const rate = rates.get(`${row.key}#${metric}`)
      return rate === undefined ? [] : [[metric, rate]]
    })),
    latency: Object.fromEntries(Object.entries(row.histograms).flatMap(([name, histogram]) => {
      const summary = summarizeLatency(histogram, name === 'wait' ? queueWait : operation)
      return summary ? [[name, summary]] : []
    })),
  })
  return {
    ...model,
    process: measure(model.process),
    listeners: model.listeners.map(measure),
    pools: model.pools.map((pool) => ({ ...measure(pool), servers: pool.servers.map(measure) })),
  }
}
