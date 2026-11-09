import { describe, expect, it } from 'vitest'
import { histogram } from './fixtures'
import { QUANTILES, quantile, summarizeLatency } from './percentiles'

const BOUNDS = [10, 50, 100, 500]
const decoded = (observations: number[]) => {
  const wire = histogram(BOUNDS, observations)
  return { count: BigInt(wire.count), sumMs: BigInt(wire.sumMs), buckets: wire.buckets.map(BigInt) }
}

describe('quantile', () => {
  // 100 observations: 50 at <=10, 30 at <=50, 15 at <=100, 5 at <=500
  const spread = decoded([...Array(50).fill(5), ...Array(30).fill(40), ...Array(15).fill(90), ...Array(5).fill(300)])

  it('is the upper bound of the first bucket holding the rank (nearest rank)', () => {
    expect(quantile(spread, BOUNDS, 0.5)).toEqual({ ms: 10, overflow: false })
    expect(quantile(spread, BOUNDS, 0.51)).toEqual({ ms: 50, overflow: false })
    expect(quantile(spread, BOUNDS, 0.9)).toEqual({ ms: 100, overflow: false })
    expect(quantile(spread, BOUNDS, 0.99)).toEqual({ ms: 500, overflow: false })
    expect(quantile(spread, BOUNDS, 1)).toEqual({ ms: 500, overflow: false })
  })

  it('can interpolate linearly inside the bucket, from 0 for the first one', () => {
    expect(quantile(spread, BOUNDS, 0.25, { interpolate: true })).toEqual({ ms: 5, overflow: false })
    expect(quantile(spread, BOUNDS, 0.65, { interpolate: true })).toEqual({ ms: 30, overflow: false })
    expect(quantile(spread, BOUNDS, 0.5, { interpolate: true })).toEqual({ ms: 10, overflow: false })
  })

  it('reports the last bound as a lower limit when the rank falls in the overflow bucket', () => {
    const slow = decoded([5, 5, 5, 9000])
    expect(quantile(slow, BOUNDS, 0.5)).toEqual({ ms: 10, overflow: false })
    expect(quantile(slow, BOUNDS, 0.99)).toEqual({ ms: 500, overflow: true })
    expect(quantile(slow, BOUNDS, 0.99, { interpolate: true })).toEqual({ ms: 500, overflow: true })
  })

  it('is unknown, not zero, without observations', () => {
    expect(quantile(decoded([]), BOUNDS, 0.5)).toBeUndefined()
  })

  it('handles a single observation and counters beyond the safe integer range', () => {
    expect(quantile(decoded([20]), BOUNDS, 0.99)).toEqual({ ms: 50, overflow: false })
    const huge = { count: 2n ** 62n, sumMs: 0n, buckets: [2n ** 61n, 2n ** 62n, 2n ** 62n, 2n ** 62n] }
    expect(quantile(huge, BOUNDS, 0.5)).toEqual({ ms: 10, overflow: false })
    expect(quantile(huge, BOUNDS, 0.51)).toEqual({ ms: 50, overflow: false })
  })

  it('rejects buckets that do not match the bounds', () => {
    expect(() => quantile({ count: 3n, sumMs: 0n, buckets: [1n, 2n] }, BOUNDS, 0.5)).toThrow(RangeError)
  })

  it.each([0, -0.1, 1.1, Number.NaN])('rejects the quantile %s', (q) => {
    expect(() => quantile(decoded([1]), BOUNDS, q)).toThrow(RangeError)
  })
})

describe('summarizeLatency', () => {
  it('gives count, mean and p50, p90 and p99', () => {
    const summary = summarizeLatency(decoded([5, 5, 40, 90]), BOUNDS)
    expect(summary).toEqual({
      count: 4n,
      meanMs: 35,
      p50: { ms: 10, overflow: false },
      p90: { ms: 100, overflow: false },
      p99: { ms: 100, overflow: false },
    })
    expect(Object.keys(QUANTILES)).toEqual(['p50', 'p90', 'p99'])
  })

  it('is undefined for an empty histogram', () => {
    expect(summarizeLatency(decoded([]), BOUNDS)).toBeUndefined()
  })
})
