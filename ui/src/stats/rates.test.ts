import { describe, expect, it } from 'vitest'
import { DEFAULT_MAX_GAP_MS, createRateTracker } from './rates'
import type { RateInput } from './rates'

const sample = (atMs: number, counters: Record<string, bigint>, generation = 1, uptimeMs = atMs): RateInput =>
  ({ atMs, generation, uptimeMs, counters: new Map(Object.entries(counters)) })
const rates = (result: { rates: ReadonlyMap<string, number> }) => Object.fromEntries(result.rates)

describe('rate tracker', () => {
  it('has no rate for the first sample', () => {
    const result = createRateTracker().update(sample(10_000, { a: 5n }))
    expect(result.outcome).toBe('first')
    expect(result.rates.size).toBe(0)
  })

  it('divides counter deltas by the elapsed seconds of the server clock', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 1000n, b: 0n }))
    const result = tracker.update(sample(12_000, { a: 1500n, b: 3n }))
    expect(result.outcome).toBe('rates')
    expect(rates(result)).toEqual({ a: 250, b: 1.5 })
  })

  it('reports a real zero when a counter did not move, not when it is unknown', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 7n }))
    expect(rates(tracker.update(sample(11_000, { a: 7n, fresh: 1n })))).toEqual({ a: 0 })
  })

  it('keeps deltas exact for counters above 2^53', () => {
    const tracker = createRateTracker()
    const base = 2n ** 63n
    tracker.update(sample(0, { a: base }))
    expect(rates(tracker.update(sample(1000, { a: base + 1000n })))).toEqual({ a: 1000 })
  })

  it('ignores a repeated snapshot and keeps the baseline for the next one', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 100n }))
    const repeated = tracker.update(sample(10_000, { a: 100n }))
    expect(repeated.outcome).toBe('same')
    expect(repeated.rates.size).toBe(0)
    expect(rates(tracker.update(sample(12_000, { a: 300n })))).toEqual({ a: 100 })
  })

  it('restarts from the new sample after a clock that went backwards', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 100n }))
    expect(tracker.update(sample(5_000, { a: 200n })).outcome).toBe('gap')
    expect(rates(tracker.update(sample(6_000, { a: 300n })))).toEqual({ a: 100 })
  })

  it('does not average over a gap longer than the limit', () => {
    const tracker = createRateTracker()
    tracker.update(sample(0, { a: 0n }))
    const late = tracker.update(sample(DEFAULT_MAX_GAP_MS + 1, { a: 1_000_000n }))
    expect(late.outcome).toBe('gap')
    expect(late.rates.size).toBe(0)
    expect(rates(tracker.update(sample(DEFAULT_MAX_GAP_MS + 2001, { a: 1_002_000n })))).toEqual({ a: 1000 })
  })

  it('takes the gap limit per call so slow pages can raise it', () => {
    const tracker = createRateTracker()
    tracker.update(sample(0, { a: 0n }))
    expect(tracker.update(sample(120_000, { a: 1200n }), 180_000).outcome).toBe('rates')
  })

  it('has no rate for one interval after a generation change, then resumes', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 900n }, 1))
    const reload = tracker.update(sample(12_000, { a: 10n }, 2))
    expect(reload.outcome).toBe('reset')
    expect(reload.rates.size).toBe(0)
    expect(rates(tracker.update(sample(14_000, { a: 110n }, 2)))).toEqual({ a: 50 })
  })

  it('treats a lower uptime as a restart even with the same generation number', () => {
    const tracker = createRateTracker()
    tracker.update(sample(10_000, { a: 900n }, 1, 600_000))
    expect(tracker.update(sample(12_000, { a: 20n }, 1, 1_000)).outcome).toBe('reset')
  })

  it('drops only the counters that went down', () => {
    const tracker = createRateTracker()
    tracker.update(sample(0, { up: 10n, down: 50n }))
    const result = tracker.update(sample(1000, { up: 20n, down: 5n }))
    expect(result.outcome).toBe('rates')
    expect(rates(result)).toEqual({ up: 10 })
  })

  it('forgets its baseline on clear', () => {
    const tracker = createRateTracker()
    tracker.update(sample(0, { a: 1n }))
    tracker.clear()
    expect(tracker.update(sample(1000, { a: 9n })).outcome).toBe('first')
  })
})
