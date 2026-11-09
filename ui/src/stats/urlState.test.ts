import { describe, expect, it } from 'vitest'
import {
  MAX_KEYS, MAX_SEARCH_LENGTH, defaultView, formatViewState, parseViewState, replaceUrlState,
} from './urlState'
import type { ViewState } from './urlState'

const everything = (): ViewState => ({
  search: 'origin café 日本 & a=b#c%',
  states: ['up', 'ejected'],
  problemsOnly: true,
  sort: { column: 'errors', direction: 'desc' },
  paused: true,
  intervalMs: 5000,
  theme: 'dark',
  expanded: ['server:web/origin-a', 'pool:a%2Fb'],
  collapsed: ['pool:api'],
})

describe('view state in the URL', () => {
  it('writes nothing for the default view', () => {
    expect(formatViewState(defaultView())).toBe('')
    expect(parseViewState('')).toEqual(defaultView())
    expect(parseViewState('?')).toEqual(defaultView())
  })

  it('round-trips every field, including text that needs escaping', () => {
    const query = formatViewState(everything())
    expect(query.startsWith('?')).toBe(true)
    expect(parseViewState(query)).toEqual(everything())
  })

  it('writes a stable, readable query', () => {
    expect(formatViewState({ ...defaultView(), search: 'web', states: ['down', 'up'], sort: { column: 'name', direction: 'asc' } }))
      .toBe('?q=web&state=up%2Cdown&sort=name')
    expect(formatViewState({ ...defaultView(), sort: { column: 'rate', direction: 'desc' }, theme: 'light' }))
      .toBe('?sort=-rate&theme=light')
  })

  it('accepts a query with or without the question mark and in any key order', () => {
    expect(parseViewState('theme=dark&q=x')).toEqual({ ...defaultView(), theme: 'dark', search: 'x' })
  })

  it('ignores unknown keys, invalid values and repeated scalar keys beyond the first', () => {
    const view = parseViewState('?x=1&state=up,bogus,up&theme=neon&interval=50&sort=bad%20name&paused=yes&q=a&q=b')
    expect(view).toEqual({ ...defaultView(), states: ['up'], search: 'a' })
  })

  it.each([['1000', 1000], ['86400000', 86_400_000], ['999', undefined], ['86400001', undefined], ['2.5', undefined], ['abc', undefined]])(
    'reads interval %s as %s', (value, expected) => {
      expect(parseViewState(`?interval=${value}`).intervalMs).toBe(expected)
    })

  it('keeps the server status chips in their canonical order and without duplicates', () => {
    expect(parseViewState('?state=ejected,up,down,up').states).toEqual(['up', 'down', 'ejected'])
  })

  it('limits the search text and the number of remembered rows', () => {
    expect(parseViewState(`?q=${'a'.repeat(MAX_SEARCH_LENGTH + 50)}`).search).toHaveLength(MAX_SEARCH_LENGTH)
    const open = Array.from({ length: MAX_KEYS + 10 }, (_, index) => `open=r${index}`).join('&')
    expect(parseViewState(`?${open}`).expanded).toHaveLength(MAX_KEYS)
  })

  it('counts characters, not UTF-16 units, when it limits the search text', () => {
    const emoji = '\u{1F600}'.repeat(MAX_SEARCH_LENGTH + 5)
    expect(parseViewState(`?q=${encodeURIComponent(emoji)}`).search).toBe('\u{1F600}'.repeat(MAX_SEARCH_LENGTH))
  })

  it('returns independent default views', () => {
    const first = defaultView()
    first.expanded.push('x')
    expect(defaultView().expanded).toEqual([])
  })
})

describe('replaceUrlState', () => {
  const target = (search: string) => {
    const calls: unknown[][] = []
    return {
      calls,
      location: { pathname: '/haproxy', search, hash: '#top' },
      history: { state: { keep: 1 }, replaceState: (...args: unknown[]) => calls.push(args) },
    }
  }

  it('replaces the query and keeps path, hash and history state', () => {
    const window = target('')
    replaceUrlState({ ...defaultView(), theme: 'dark' }, window)
    expect(window.calls).toEqual([[{ keep: 1 }, '', '/haproxy?theme=dark#top']])
  })

  it('does nothing when the URL already says it', () => {
    const window = target('?theme=dark')
    replaceUrlState({ ...defaultView(), theme: 'dark' }, window)
    expect(window.calls).toEqual([])
  })

  it('removes the query for the default view', () => {
    const window = target('?theme=dark')
    replaceUrlState(defaultView(), window)
    expect(window.calls).toEqual([[{ keep: 1 }, '', '/haproxy#top']])
  })
})
