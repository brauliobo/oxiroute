import { describe, expect, it } from 'vitest'

import {
  NO_DATA_YET, formatBytes, formatCount, formatSnapshotTime, formatTelemetryAge, formatTelemetryDuration, formatTime,
  pluralize, presentApiError, shortRevision,
} from './formatters'

describe('shared presentation formatters', () => {
  it('formats every time as UTC text that does not depend on the locale or the zone', () => {
    expect(formatTime(Date.UTC(2026, 7, 10, 12, 34, 56))).toBe('2026-08-10 12:34:56 UTC')
  })

  it('shows a snapshot that was never taken as no data, not as the epoch', () => {
    expect(formatSnapshotTime(0)).toBe(NO_DATA_YET)
    expect(formatSnapshotTime(1_750_000_000_000)).toBe('2025-06-15 15:06:40 UTC')
  })

  it('shares locale-independent counts, IEC sizes, durations and ages with the stats page', () => {
    expect(formatCount('12345')).toBe('12,345')
    expect(formatBytes('1572864')).toBe('1.5 MiB')
    expect(formatTelemetryDuration(90_061_000)).toBe('1d 1h')
    expect(formatTelemetryAge(10_000, 15_000)).toBe('5 s ago')
    expect(formatTelemetryAge(20_000, 15_000)).toBe('just now')
  })

  it('pluralizes a count with its own word', () => {
    expect(pluralize(0, 'server')).toBe('0 servers')
    expect(pluralize(1, 'server')).toBe('1 server')
    expect(pluralize(2, 'application')).toBe('2 applications')
    expect(pluralize(1, 'discontinuity', 'discontinuities')).toBe('1 discontinuity')
    expect(pluralize(3, 'discontinuity', 'discontinuities')).toBe('3 discontinuities')
    expect(pluralize('1', 'segment')).toBe('1 segment')
    expect(pluralize('18446744073709551615', 'drop')).toBe('18,446,744,073,709,551,615 drops')
    expect(pluralize(1500, 'listener')).toBe('1,500 listeners')
  })

  it('shortens revisions and preserves the absent revision label', () => {
    expect(shortRevision('0123456789abcdef0123456789abcdef')).toBe('0123456789ab...cdef')
    expect(shortRevision('short')).toBe('short')
    expect(shortRevision(null)).toBe('None')
  })

  it('presents Error messages without inventing messages for unknown failures', () => {
    expect(presentApiError(new Error('request failed'), 'fallback')).toBe('request failed')
    expect(presentApiError({ message: 'untrusted' }, 'fallback')).toBe('fallback')
  })
})
