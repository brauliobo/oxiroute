import { describe, expect, it } from 'vitest'

import {
  formatBytes, formatClockTime, formatCount, formatTelemetryAge, formatTelemetryDuration, formatTime, presentApiError,
  shortRevision,
} from './formatters'

describe('shared presentation formatters', () => {
  it('preserves date-time and clock-only presentation contracts', () => {
    const timestamp = Date.UTC(2026, 7, 10, 12, 34, 56)
    expect(formatTime(timestamp)).toBe(new Intl.DateTimeFormat(undefined, {
      dateStyle: 'short',
      timeStyle: 'medium',
    }).format(timestamp))
    expect(formatClockTime(timestamp)).toBe(new Intl.DateTimeFormat(undefined, {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    }).format(timestamp))
  })

  it('shares locale-independent counts, IEC sizes, durations and ages with the stats page', () => {
    expect(formatCount('12345')).toBe('12,345')
    expect(formatBytes('1572864')).toBe('1.5 MiB')
    expect(formatTelemetryDuration(90_061_000)).toBe('1d 1h')
    expect(formatTelemetryAge(10_000, 15_000)).toBe('5 s ago')
    expect(formatTelemetryAge(20_000, 15_000)).toBe('just now')
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
