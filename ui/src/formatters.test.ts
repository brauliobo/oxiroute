import { describe, expect, it } from 'vitest'

import { formatClockTime, formatTime, presentApiError, shortRevision } from './formatters'

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
