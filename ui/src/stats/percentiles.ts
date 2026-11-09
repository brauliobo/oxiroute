// Percentiles from the cumulative lifetime histograms of the snapshot. `buckets[i]` counts observations at or below
// `bounds[i]`; `count` also holds the ones above the last bound (the overflow bucket).
//
// Rules
// - Nearest rank: the q-quantile is the first bucket whose cumulative count reaches ceil(q * count).
// - By default the answer is that bucket's upper bound, i.e. "at most this many ms" (the contract's reading).
//   `interpolate` spreads the bucket's observations evenly between the previous bound (0 for the first bucket) and
//   its upper bound, a smoother estimate that is still not an exact value.
// - A rank beyond the last bucket is the overflow: the answer is the last bound with `overflow: true` ("more than").
// - Without observations there is no answer (undefined), never 0.

export const QUANTILES = { p50: 0.5, p90: 0.9, p99: 0.99 } as const

export interface HistogramCounts {
  count: bigint
  sumMs: bigint
  buckets: readonly bigint[]
}

export interface Quantile {
  ms: number
  /** The quantile lies above `ms` (beyond the last bucket bound). */
  overflow: boolean
}

export interface LatencySummary {
  count: bigint
  meanMs: number
  p50: Quantile
  p90: Quantile
  p99: Quantile
}

const SCALE = 1_000_000n

export function quantile(
  histogram: HistogramCounts,
  bounds: readonly number[],
  q: number,
  { interpolate = false } = {},
): Quantile | undefined {
  if (!(q > 0 && q <= 1)) throw new RangeError(`quantile must be in (0, 1]: ${q}`)
  if (histogram.buckets.length !== bounds.length) {
    throw new RangeError(`${histogram.buckets.length} buckets for ${bounds.length} bounds`)
  }
  const { count, buckets } = histogram
  if (count === 0n || bounds.length === 0) return undefined
  const rank = (count * BigInt(Math.round(q * Number(SCALE))) + SCALE - 1n) / SCALE
  const index = buckets.findIndex((cumulative) => cumulative >= rank)
  if (index < 0) return { ms: bounds[bounds.length - 1]!, overflow: true }
  const upper = bounds[index]!
  if (!interpolate) return { ms: upper, overflow: false }
  const lower = index === 0 ? 0 : bounds[index - 1]!
  const below = index === 0 ? 0n : buckets[index - 1]!
  const fraction = Number(rank - below) / Number(buckets[index]! - below)
  return { ms: lower + (upper - lower) * fraction, overflow: false }
}

export function summarizeLatency(
  histogram: HistogramCounts,
  bounds: readonly number[],
  options?: { interpolate?: boolean },
): LatencySummary | undefined {
  if (histogram.count === 0n) return undefined
  const [p50, p90, p99] = Object.values(QUANTILES).map((q) => quantile(histogram, bounds, q, options)!)
  return { count: histogram.count, meanMs: Number(histogram.sumMs) / Number(histogram.count), p50: p50!, p90: p90!, p99: p99! }
}
