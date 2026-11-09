import { describe, expect, it } from 'vitest'
import { UNKNOWN } from '../format'
import { ICON_PATHS, toneOf } from './status'
import { SPARK_SAMPLES, sparkShape } from './spark'
import { histogramBars } from './histogram'
import {
  anchorId, byteRate, bytes, count, duration, exact, percent, quantileText, quantileTitle, rate, responseShares, sessions,
  since, text, timestamp, yesNo,
} from './present'

describe('present', () => {
  it('renders every absent value as the em dash and never as zero', () => {
    const cells = [count(), exact(), bytes(), rate(), byteRate(), duration(), percent(), text(), text(''), yesNo(),
      since(1000), timestamp(), quantileText()]
    expect(cells).toEqual(cells.map(() => UNKNOWN))
    expect(UNKNOWN).toBe('—')
  })

  it('renders known values, zero included', () => {
    expect(count(0n)).toBe('0')
    expect(count(1_234_000n)).toBe('1.2M')
    expect(exact(1234567n)).toBe('1,234,567')
    expect(rate(0)).toBe('0/s')
    expect(sessions(3, 100)).toBe('3 / 100')
    expect(sessions(3)).toBe('3 / —')
    expect(yesNo(false)).toBe('no')
  })

  it('measures ages at the sample time, never below zero', () => {
    expect(since(10_000, 4_000)).toBe('6 s ago')
    expect(since(10_000, 12_000)).toBe('just now')
  })

  it('reads a percentile as a bucket bound', () => {
    expect(quantileText({ ms: 50, overflow: false })).toBe('50 ms')
    expect(quantileText({ ms: 5000, overflow: true })).toBe('> 5 s')
    expect(quantileTitle({ ms: 50, overflow: false })).toContain('at most 50 ms')
    expect(quantileTitle({ ms: 5000, overflow: true })).toContain('more than 5 s')
    expect(quantileTitle()).toBe('no observations yet')
  })

  it('shares response classes of all classified responses, and has none without responses', () => {
    const none = { '1xx': 0n, '2xx': 0n, '3xx': 0n, '4xx': 0n, '5xx': 0n }
    expect(responseShares(none)).toEqual([])
    const shares = responseShares({ ...none, '2xx': 900n, '4xx': 75n, '5xx': 25n })
    expect(shares.map((entry) => entry.share)).toEqual([0, 90, 0, 7.5, 2.5])
    expect(shares[1]!.count).toBe(900n)
  })

  it('makes DOM ids from arbitrary row keys', () => {
    expect(anchorId('pool:we b/<x>%')).toMatch(/^[A-Za-z0-9_-]+$/)
    expect(anchorId('pool:a')).not.toBe(anchorId('pool:b'))
    expect(anchorId('pool:a b')).not.toBe(anchorId('pool:a_b'))
  })
})

describe('status', () => {
  it('gives every status a tone and every tone its own shape', () => {
    expect(['up', 'healthy', 'listening'].map(toneOf)).toEqual(['ok', 'ok', 'ok'])
    expect(['degraded', 'drain'].map(toneOf)).toEqual(['warn', 'warn'])
    expect(['down', 'ejected', 'failed', 'expired'].map(toneOf)).toEqual(['bad', 'bad', 'bad', 'bad'])
    expect(toneOf('maint')).toBe('info')
    expect(['unchecked', 'unknown', 'empty', 'anything'].map(toneOf)).toEqual(['neutral', 'neutral', 'neutral', 'neutral'])
    expect(new Set(Object.values(ICON_PATHS)).size).toBe(5)
  })
})

describe('sparkShape', () => {
  it('has no shape below two known samples', () => {
    expect(sparkShape([])).toBeUndefined()
    expect(sparkShape([undefined, 4, undefined])).toBeUndefined()
  })

  it('draws known samples on a zero baseline, right-aligned, with gaps for unknown ones', () => {
    const shape = sparkShape([0, 10, undefined, 5])!
    expect(shape.known).toEqual([0, 10, 5])
    expect(shape.max).toBe(10)
    expect(shape.last).toBe(5)
    expect(shape.d.match(/M/g)).toHaveLength(2)
    const ys = [...shape.d.matchAll(/[ML][\d.]+ ([\d.]+)/g)].map((match) => Number(match[1]))
    expect(ys[0]).toBeGreaterThan(ys[1]!)
    expect(shape.d.startsWith('M')).toBe(true)
  })

  it('draws a flat series on the baseline and keeps only the last samples', () => {
    expect(sparkShape([0, 0, 0])!.d).toMatch(/^M[\d.]+ 14\.5L/)
    const long = Array.from({ length: SPARK_SAMPLES + 40 }, (_, index) => index)
    expect(sparkShape(long)!.known).toHaveLength(SPARK_SAMPLES)
  })
})

describe('histogramBars', () => {
  it('turns cumulative buckets into per-bucket bars with an overflow bar', () => {
    const bars = histogramBars({ count: 10n, sumMs: 0n, buckets: [1n, 4n, 8n] }, [1, 5, 10])
    expect(bars.map((bar) => [bar.label, bar.count])).toEqual([['≤ 1 ms', 1n], ['≤ 5 ms', 3n], ['≤ 10 ms', 4n], ['> 10 ms', 2n]])
    expect(bars.map((bar) => bar.share)).toEqual([25, 75, 100, 50])
  })

  it('has empty bars without observations', () => {
    expect(histogramBars({ count: 0n, sumMs: 0n, buckets: [0n] }, [1]).map((bar) => bar.share)).toEqual([0, 0])
  })
})
