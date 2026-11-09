import { formatDuration } from '../format'
import type { HistogramCounts } from '../percentiles'

export interface Bar {
  label: string
  count: bigint
  /** 0 to 100, relative to the fullest bucket. */
  share: number
}

/** The cumulative lifetime histogram as one bar per bucket: `<= bound`, then the overflow `> last bound`. */
export function histogramBars(histogram: HistogramCounts, bounds: readonly number[]): Bar[] {
  const cumulative = [...histogram.buckets, histogram.count]
  const labels = [...bounds.map((bound) => `≤ ${formatDuration(bound)}`), `> ${formatDuration(bounds.at(-1) ?? 0)}`]
  const counts = cumulative.map((total, index) => total - (index === 0 ? 0n : cumulative[index - 1]!))
  const fullest = counts.reduce((most, count) => (count > most ? count : most), 0n)
  return counts.map((count, index) => ({
    label: labels[index]!,
    count,
    share: fullest === 0n ? 0 : Number((count * 10_000n) / fullest) / 100,
  }))
}
