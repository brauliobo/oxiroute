import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DecodeError, UnsupportedSchemaError, decodeSnapshot } from './decode'
import type { StatsSnapshot } from './decode'
import { BASE_SAMPLED_AT_MS, counter, wireDocument } from './fixtures'
import type { StatsDocument } from './generated/statsPage'
import { createStatsStore } from './store'
import type { StatsStore } from './store'

// A sample `seconds` after the base one: the server clock advances, the counters grow by `requests`.
const sample = (seconds: number, requests = 0, overrides: Partial<StatsDocument> = {}): StatsSnapshot => {
  const wire = wireDocument({ sampledAtUnixMs: BASE_SAMPLED_AT_MS + seconds * 1000, ...overrides })
  const server = wire.pools[0]!.servers[0]!
  server.traffic.sessions.total = counter(1000 + requests)
  return decodeSnapshot(wire)
}

function harness(results: Array<StatsSnapshot | Error>, search = '') {
  const queue = [...results]
  const replaced: string[] = []
  const location = { pathname: '/haproxy', search, hash: '' }
  const history = {
    state: null,
    replaceState: (_state: unknown, _title: string, url: string) => {
      replaced.push(url)
      location.search = url.includes('?') ? url.slice(url.indexOf('?')) : ''
    },
  }
  const signals: AbortSignal[] = []
  const store = createStatsStore({
    load: async (signal) => {
      signals.push(signal)
      const next = queue.length > 1 ? queue.shift()! : queue[0]!
      if (next instanceof Error) throw next
      return next
    },
    location: { location, history },
    random: () => 0.5,
  })
  return { store, replaced, signals }
}

const tick = (ms: number) => vi.advanceTimersByTimeAsync(ms)
const requestsRate = (store: StatsStore) => store.model.value?.pools[0]?.servers[0]?.rates.requests

beforeEach(() => { vi.useFakeTimers({ now: 5_000_000 }) })
afterEach(() => { vi.useRealTimers() })

describe('loading', () => {
  it('starts empty and loading, then holds the decoded snapshot, the model and the summary', async () => {
    const { store } = harness([sample(0)])
    expect(store.status.value).toBe('loading')
    expect(store.snapshot.value).toBeUndefined()
    expect(store.model.value).toBeUndefined()
    store.start()
    await tick(0)
    expect(store.status.value).toBe('live')
    expect(store.snapshot.value?.process.version).toBe('6.3.0')
    expect(store.model.value?.summary.servers).toEqual({ up: 2, total: 3 })
    expect(store.intervalMs.value).toBe(2000)
    store.dispose()
  })

  it('polls at the interval hint and ages the data with the clock', async () => {
    const { store } = harness([sample(0), sample(2, 10)])
    store.start()
    await tick(0)
    expect(store.ageMs.value).toBe(0)
    await tick(1000)
    expect(store.ageMs.value).toBe(1000)
    await tick(1000)
    expect(store.ageMs.value).toBe(0)
    store.dispose()
  })
})

describe('rates and history', () => {
  it('has no rate after one sample and per-second rates after two', async () => {
    const { store } = harness([sample(0), sample(2, 100)])
    store.start()
    await tick(0)
    expect(requestsRate(store)).toBeUndefined()
    expect(store.rateOutcome.value).toBe('first')
    await tick(2000)
    expect(requestsRate(store)).toBe(50)
    expect(store.rateOutcome.value).toBe('rates')
    store.dispose()
  })

  it('records a series per key: gauges from the first sample, rates from the second, gaps where unknown', async () => {
    const { store } = harness([sample(0), sample(2, 100), sample(4, 300)])
    store.start()
    await tick(0)
    await tick(2000)
    await tick(2000)
    const key = 'server:web/origin-a'
    expect(store.series(`${key}#active`)).toEqual([3, 3, 3])
    expect(store.series(`${key}#requests`)).toEqual([undefined, 50, 100])
    expect(store.seriesTimes()).toEqual([BASE_SAMPLED_AT_MS, BASE_SAMPLED_AT_MS + 2000, BASE_SAMPLED_AT_MS + 4000])
    store.dispose()
  })

  it('treats a repeated snapshot as no news: no new point, the rate stays', async () => {
    const { store } = harness([sample(0), sample(2, 100), sample(2, 100)])
    store.start()
    await tick(2000)
    await tick(2000)
    expect(store.rateOutcome.value).toBe('rates')
    expect(requestsRate(store)).toBe(50)
    expect(store.seriesTimes()).toHaveLength(2)
    store.dispose()
  })

  it('shows no rate for the interval after a generation change, while rows keep their keys', async () => {
    const generation = { ...wireDocument().generation, number: 5 }
    const { store } = harness([sample(0), sample(2, 100), sample(4, 0, { generation }), sample(6, 40, { generation })])
    store.start()
    await tick(4000)
    expect(store.rateOutcome.value).toBe('reset')
    expect(requestsRate(store)).toBeUndefined()
    expect(store.model.value?.pools[0]?.servers[0]?.key).toBe('server:web/origin-a')
    await tick(2000)
    expect(requestsRate(store)).toBe(20)
    store.dispose()
  })
})

describe('failures and staleness', () => {
  it('keeps the last snapshot, retries, and goes stale after three intervals', async () => {
    const { store } = harness([sample(0), new TypeError('Failed to fetch')])
    store.start()
    await tick(0)
    await tick(2000)
    expect(store.status.value).toBe('retrying')
    expect(store.failure.value?.kind).toBe('network')
    expect(store.snapshot.value).toBeDefined()
    await tick(10_000)
    expect(store.status.value).toBe('stale')
    expect(store.model.value?.pools).toHaveLength(2)
    store.dispose()
  })

  it('is an error, not live, when nothing was ever loaded', async () => {
    const { store } = harness([new DecodeError('pools', 'expected a list')])
    store.start()
    await tick(0)
    expect(store.status.value).toBe('error')
    expect(store.model.value).toBeUndefined()
    store.dispose()
  })

  it('is unsupported on another schema version and stops polling', async () => {
    const { store, signals } = harness([new UnsupportedSchemaError(2)])
    store.start()
    await tick(60_000)
    expect(store.status.value).toBe('unsupported')
    expect(signals).toHaveLength(1)
    store.dispose()
  })

  it('recovers to live on the next good snapshot', async () => {
    const { store } = harness([new TypeError('x'), sample(0)])
    store.start()
    await tick(0)
    expect(store.status.value).toBe('error')
    await tick(2000)
    expect(store.status.value).toBe('live')
    expect(store.failure.value).toBeUndefined()
    store.dispose()
  })
})

describe('pause, interval and refresh', () => {
  it('pausing stops requests and reports paused; resuming loads at once', async () => {
    const { store, signals } = harness([sample(0), sample(2, 10)])
    store.start()
    await tick(0)
    store.setPaused(true)
    expect(store.status.value).toBe('paused')
    await tick(60_000)
    expect(signals).toHaveLength(1)
    store.togglePause()
    await tick(0)
    expect(signals).toHaveLength(2)
    expect(store.status.value).toBe('live')
    store.dispose()
  })

  it('takes the viewer interval over the hint', async () => {
    const { store, signals } = harness([sample(0)])
    store.start()
    await tick(0)
    store.setIntervalMs(5000)
    expect(store.intervalMs.value).toBe(5000)
    await tick(4999)
    expect(signals).toHaveLength(1)
    await tick(1)
    expect(signals).toHaveLength(2)
    store.dispose()
  })

  it('refresh loads now', async () => {
    const { store, signals } = harness([sample(0)])
    store.start()
    await tick(0)
    store.refresh()
    await tick(0)
    expect(signals).toHaveLength(2)
    store.dispose()
  })

  it('dispose aborts the request in flight and leaves no timers', async () => {
    const { store, signals } = harness([sample(0)])
    store.start()
    store.dispose()
    expect(signals[0]!.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })
})

describe('view state', () => {
  it('starts from the URL query; a paused page still loads once', async () => {
    const { store, signals } = harness([sample(0)], '?q=origin-b&paused=1&interval=5000&theme=dark')
    expect(store.view.value).toMatchObject({ search: 'origin-b', paused: true, intervalMs: 5000, theme: 'dark' })
    store.start()
    await tick(60_000)
    expect(signals).toHaveLength(1)
    expect(store.status.value).toBe('paused')
    expect(store.model.value).toBeDefined()
    store.dispose()
  })

  it('writes every change back to the address bar without a history entry', () => {
    const { store, replaced } = harness([sample(0)])
    store.setView({ search: 'web', problemsOnly: true })
    expect(replaced.at(-1)).toBe('/haproxy?q=web&problems=1')
    store.setView({ search: '', problemsOnly: false })
    expect(replaced.at(-1)).toBe('/haproxy')
    store.dispose()
  })

  it('toggles open rows and folded pools, and pause and interval end up in the URL', () => {
    const { store, replaced } = harness([sample(0)])
    store.toggleExpanded('server:web/origin-a')
    store.toggleCollapsed('pool:api')
    expect(replaced.at(-1)).toBe('/haproxy?open=server%3Aweb%2Forigin-a&closed=pool%3Aapi')
    store.toggleExpanded('server:web/origin-a')
    store.setPaused(true)
    store.setIntervalMs(5000)
    expect(replaced.at(-1)).toBe('/haproxy?paused=1&interval=5000&closed=pool%3Aapi')
    store.dispose()
  })

  it('exposes the rows the view shows, filtered and sorted', async () => {
    const { store } = harness([sample(0)], '?q=origin-b')
    store.start()
    await tick(0)
    expect(store.visible.value?.pools.map((pool) => [pool.name, pool.servers.map((server) => server.name)])).toEqual([['web', ['origin-b']]])
    store.setView({ search: '', sort: { column: 'name', direction: 'asc' } })
    expect(store.visible.value?.pools.map((pool) => pool.name)).toEqual(['api', 'web'])
    store.dispose()
  })
})
