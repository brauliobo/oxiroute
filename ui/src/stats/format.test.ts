import { describe, expect, it } from 'vitest'
import {
  UNKNOWN, formatAge, formatByteRate, formatBytes, formatCount, formatDuration, formatExact, formatPercent,
  formatRate, formatTimestamp, truncate,
} from './format'

describe('formatBytes', () => {
  it.each([
    [0, '0 B'], [1, '1 B'], [1023, '1023 B'], [1024, '1 KiB'], [1536, '1.5 KiB'], [10 * 1024, '10 KiB'],
    [1048575, '1 MiB'], [3.4 * 1024 ** 3, '3.4 GiB'], [2 ** 60, '1 EiB'], [2 ** 70, '1024 EiB'],
  ])('%s is %s', (value, text) => expect(formatBytes(value)).toBe(text))

  it('reads bigint counters, also beyond the safe integer range', () => {
    expect(formatBytes(2n ** 40n)).toBe('1 TiB')
    expect(formatBytes(2n ** 64n - 1n)).toBe('16 EiB')
  })

  it('keeps the sign', () => expect(formatBytes(-1536)).toBe('-1.5 KiB'))
})

describe('formatCount', () => {
  it.each([
    [0, '0'], [999, '999'], [1000, '1k'], [1234, '1.2k'], [12_000, '12k'], [999_999, '1M'], [1_234_567, '1.2M'],
    [5.6e9, '5.6G'], [7e12, '7T'],
  ])('%s is %s', (value, text) => expect(formatCount(value)).toBe(text))

  it('reads bigints', () => expect(formatCount(1_500_000n)).toBe('1.5M'))
})

describe('formatExact', () => {
  it.each([[0, '0'], [999, '999'], [1000, '1,000'], [1234567, '1,234,567']])('%s is %s', (value, text) => {
    expect(formatExact(value)).toBe(text)
  })

  it('keeps every digit of a u64', () => expect(formatExact(18446744073709551615n)).toBe('18,446,744,073,709,551,615'))
})

describe('formatRate', () => {
  it.each([[0, '0/s'], [0.004, '<0.01/s'], [0.25, '0.25/s'], [3.5, '3.5/s'], [120, '120/s'], [1250, '1.3k/s']])(
    '%s is %s', (value, text) => expect(formatRate(value)).toBe(text))

  it('formats byte rates', () => expect(formatByteRate(1.5 * 1024 ** 2)).toBe('1.5 MiB/s'))
})

describe('formatDuration', () => {
  it.each([
    [0, '0 ms'], [0.5, '0.5 ms'], [8.54, '8.5 ms'], [850, '850 ms'], [999.9, '999 ms'], [1200, '1.2 s'], [45_900, '45 s'],
    [185_000, '3m 5s'], [3_599_999, '59m 59s'], [7_800_000, '2h 10m'], [4 * 86_400_000 + 3 * 3_600_000, '4d 3h'],
  ])('%s ms is %s', (value, text) => expect(formatDuration(value)).toBe(text))

  it('keeps the sign', () => expect(formatDuration(-1200)).toBe('-1.2 s'))
})

describe('formatAge', () => {
  it.each([[0, 'just now'], [999, 'just now'], [4200, '4.2 s ago'], [185_000, '3m 5s ago']])('%s is %s', (value, text) => {
    expect(formatAge(value)).toBe(text)
  })
})

describe('formatTimestamp', () => {
  it('prints UTC independent of the machine locale and zone', () => {
    expect(formatTimestamp(1_800_000_000_000)).toBe('2027-01-15 08:00:00 UTC')
  })

  it('applies a fixed offset', () => {
    expect(formatTimestamp(1_800_000_000_000, 90)).toBe('2027-01-15 09:30:00 +01:30')
    expect(formatTimestamp(1_800_000_000_000, -300)).toBe('2027-01-15 03:00:00 -05:00')
  })
})

describe('formatPercent', () => {
  it.each([[0, '0%'], [12.5, '12.5%'], [99.96, '100%'], [3, '3%']])('%s is %s', (value, text) => {
    expect(formatPercent(value)).toBe(text)
  })
})

describe('truncate', () => {
  it('leaves short text alone', () => expect(truncate('origin-a', 8)).toBe('origin-a'))

  it('cuts to the limit with an ellipsis', () => expect(truncate('origin-a.example.com', 10)).toBe('origin-a.…'))

  it('never splits a surrogate pair or a combined emoji', () => {
    expect(truncate('ab\u{1F600}cd', 4)).toBe('ab\u{1F600}…')
    const family = '\u{1F468}‍\u{1F469}‍\u{1F467}'
    expect(truncate(`x${family}y${family}`, 3)).toBe(`x${family}…`)
    expect(truncate('ééé', 3)).toBe('ééé')
  })

  it('rejects a limit below one', () => expect(() => truncate('x', 0)).toThrow(RangeError))
})

it('has a single marker for unknown values', () => expect(UNKNOWN).toBe('—'))
