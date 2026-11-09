// Per-second rates from counter deltas between two snapshots. Time is the server's `sampledAtUnixMs`, so a skewed
// browser clock or a late poll cannot distort a rate, and a snapshot served twice from the server cache is recognised.
// A rate that cannot be known is absent from the result; it is never zero. Zero means the counter did not move.

export const DEFAULT_MAX_GAP_MS = 60_000

export interface RateInput {
  atMs: number
  generation: number
  uptimeMs: number
  counters: ReadonlyMap<string, bigint>
}

/**
 * `first`  no earlier sample, no rates
 * `rates`  a delta over a normal interval
 * `same`   the very same sample again (equal time): nothing to record, the baseline is kept
 * `gap`    the clock went backwards or more than the limit passed: no rates, the sample becomes the baseline
 * `reset`  the generation changed or the process restarted (uptime went down): counters may have restarted, so no
 *          rates for this interval, the sample becomes the baseline
 */
export type RateOutcome = 'first' | 'rates' | 'same' | 'gap' | 'reset'

export interface RateResult {
  outcome: RateOutcome
  rates: ReadonlyMap<string, number>
}

export interface RateTracker {
  /** `maxGapMs`: longest interval a rate may be averaged over. */
  update(next: RateInput, maxGapMs?: number): RateResult
  clear(): void
}

export function createRateTracker(): RateTracker {
  let baseline: RateInput | undefined
  return {
    update(next, maxGapMs = DEFAULT_MAX_GAP_MS) {
      const result = compare(baseline, next, maxGapMs)
      if (result.outcome !== 'same') baseline = next
      return result
    },
    clear() { baseline = undefined },
  }
}

const NONE: ReadonlyMap<string, number> = new Map()

function compare(previous: RateInput | undefined, next: RateInput, maxGapMs: number): RateResult {
  if (!previous) return { outcome: 'first', rates: NONE }
  const elapsedMs = next.atMs - previous.atMs
  if (elapsedMs === 0) return { outcome: 'same', rates: NONE }
  if (elapsedMs < 0 || elapsedMs > maxGapMs) return { outcome: 'gap', rates: NONE }
  if (next.generation !== previous.generation || next.uptimeMs < previous.uptimeMs) return { outcome: 'reset', rates: NONE }
  const rates = new Map<string, number>()
  for (const [key, value] of next.counters) {
    const before = previous.counters.get(key)
    if (before !== undefined && value >= before) rates.set(key, Number(value - before) * 1000 / elapsedMs)
  }
  return { outcome: 'rates', rates }
}
