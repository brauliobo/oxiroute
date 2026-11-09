import { describe, expect, it } from 'vitest'
import { csvUrl, jsonUrl } from './links'

describe('export links', () => {
  it('are below the page path, with or without a trailing slash', () => {
    expect(csvUrl('/haproxy')).toBe('/haproxy/stats.csv')
    expect(jsonUrl('/haproxy/')).toBe('/haproxy/snapshot.json')
    expect(csvUrl('/')).toBe('/stats.csv')
  })
})
