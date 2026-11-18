import { describe, expect, it } from 'vitest'
import { expiryBadge, expiryPhrase } from './expiry'

const NOW = Date.UTC(2026, 6, 12)
const DAY = 86_400_000
const HOUR = 3_600_000

describe('certificate expiry', () => {
  it.each([
    [20 * DAY, 'expires in 20 days'],
    [DAY, 'expires in 1 day'],
    [5 * HOUR, 'expires in 5 hours'],
    [HOUR, 'expires in 1 hour'],
    [10 * 60_000, 'expires in less than an hour'],
    [-3 * DAY, 'expired 3 days ago'],
    [-HOUR, 'expired 1 hour ago'],
    [-60_000, 'expired less than an hour ago'],
  ])('phrases %d ms from now as "%s"', (offset, phrase) => {
    expect(expiryPhrase(NOW + offset, NOW)).toBe(phrase)
  })

  it('warns within thirty days, reports an expired certificate and stays quiet otherwise', () => {
    expect(expiryBadge(NOW + 20 * DAY, NOW)).toEqual({ tone: 'warn', label: 'Expires soon' })
    expect(expiryBadge(NOW + 30 * DAY, NOW)).toEqual({ tone: 'warn', label: 'Expires soon' })
    expect(expiryBadge(NOW + 31 * DAY, NOW)).toBeNull()
    expect(expiryBadge(NOW - 1, NOW)).toEqual({ tone: 'bad', label: 'Expired' })
  })
})
