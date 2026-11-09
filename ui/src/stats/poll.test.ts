import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DecodeError, UnsupportedSchemaError, decodeSnapshot } from './decode'
import type { StatsSnapshot } from './decode'
import { wireDocument } from './fixtures'
import {
  BACKOFF_CAP_MS, HttpStatusError, STALE_FLOOR_MS, StatsUnavailableError, createPoller, fetchSnapshot, snapshotUrl,
} from './poll'
import type { PollState, PollerOptions } from './poll'

const snapshot = (intervalHintMs = 2000): StatsSnapshot => decodeSnapshot(wireDocument({ intervalHintMs }))

interface Harness {
  calls: AbortSignal[]
  snapshots: Array<[StatsSnapshot, number]>
  states: PollState[]
  results: Array<() => Promise<StatsSnapshot>>
  poller: ReturnType<typeof createPoller>
  state(): PollState
}

// `results` is consumed one entry per request; the last one repeats.
function harness(results: Array<StatsSnapshot | Error | (() => Promise<StatsSnapshot>)>, options: Partial<PollerOptions> = {}): Harness {
  const calls: AbortSignal[] = []
  const queue = results.map((result) => (typeof result === 'function' ? result : () => (result instanceof Error ? Promise.reject(result) : Promise.resolve(result))))
  const snapshots: Harness['snapshots'] = []
  const states: PollState[] = []
  const poller = createPoller({
    load: (signal) => {
      calls.push(signal)
      return (queue.length > 1 ? queue.shift()! : queue[0]!)()
    },
    onSnapshot: (value, receivedAtMs) => snapshots.push([value, receivedAtMs]),
    onState: (state) => states.push(state),
    random: () => 0.5,
    ...options,
  })
  return { calls, snapshots, states, results: queue, poller, state: () => states.at(-1)! }
}

beforeEach(() => { vi.useFakeTimers({ now: 1_000_000 }) })
afterEach(() => { vi.useRealTimers() })

describe('polling', () => {
  it('loads at once on start and then every intervalHintMs of the latest snapshot', async () => {
    const h = harness([snapshot(3000)])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.calls).toHaveLength(1)
    expect(h.snapshots[0]![1]).toBe(1_000_000)
    await vi.advanceTimersByTimeAsync(2999)
    expect(h.calls).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.calls).toHaveLength(2)
    h.poller.stop()
  })

  it('follows a changed hint and never polls faster than one second', async () => {
    const h = harness([snapshot(5000), snapshot(10)])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(5000)
    expect(h.calls).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(999)
    expect(h.calls).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.calls).toHaveLength(3)
    h.poller.stop()
  })

  it('prefers the viewer interval over the hint and lets it be changed while running', async () => {
    const h = harness([snapshot(2000)], { intervalMs: 5000 })
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(4999)
    expect(h.calls).toHaveLength(1)
    h.poller.setIntervalMs(1000)
    await vi.advanceTimersByTimeAsync(1000)
    expect(h.calls).toHaveLength(2)
    h.poller.setIntervalMs(undefined)
    await vi.advanceTimersByTimeAsync(2000)
    expect(h.calls).toHaveLength(3)
    h.poller.stop()
  })

  it('has one request in flight at a time and refresh() loads immediately', async () => {
    let finish!: (value: StatsSnapshot) => void
    const slow = () => new Promise<StatsSnapshot>((resolve) => { finish = resolve })
    const h = harness([slow, snapshot()])
    h.poller.start()
    h.poller.refresh()
    expect(h.calls).toHaveLength(1)
    expect(h.state().loading).toBe(true)
    finish(snapshot())
    await vi.advanceTimersByTimeAsync(0)
    expect(h.state().loading).toBe(false)
    h.poller.refresh()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.calls).toHaveLength(2)
    h.poller.stop()
  })
})

describe('pause and stop', () => {
  it('pause aborts the request in flight and stops the schedule; resume loads at once', async () => {
    let finish!: (value: StatsSnapshot) => void
    const h = harness([() => new Promise<StatsSnapshot>((resolve) => { finish = resolve }), snapshot()])
    h.poller.start()
    h.poller.pause()
    expect(h.calls[0]!.aborted).toBe(true)
    expect(h.state()).toMatchObject({ phase: 'paused', loading: false })
    finish(snapshot())
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.snapshots).toHaveLength(0)
    expect(h.calls).toHaveLength(1)
    h.poller.resume()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.calls).toHaveLength(2)
    expect(h.state().phase).toBe('running')
    expect(h.snapshots).toHaveLength(1)
    h.poller.stop()
  })

  it('can start paused: it loads once so the page has data, then waits', async () => {
    const h = harness([snapshot()], { paused: true })
    h.poller.start()
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.calls).toHaveLength(1)
    expect(h.snapshots).toHaveLength(1)
    expect(h.state()).toMatchObject({ phase: 'paused', stale: false })
    h.poller.stop()
  })

  it('refresh loads once while paused and schedules nothing; a failed manual load is not retried', async () => {
    const h = harness([snapshot(), new TypeError('Failed to fetch')])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    h.poller.pause()
    h.poller.refresh()
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.calls).toHaveLength(2)
    expect(h.state().failure).toMatchObject({ kind: 'network', attempts: 1 })
    expect('retryInMs' in h.state().failure!).toBe(false)
    h.poller.stop()
  })

  it('stop aborts, drops a late answer and leaves no timers', async () => {
    let finish!: (value: StatsSnapshot) => void
    const h = harness([() => new Promise<StatsSnapshot>((resolve) => { finish = resolve })])
    h.poller.start()
    h.poller.stop()
    expect(h.calls[0]!.aborted).toBe(true)
    finish(snapshot())
    await vi.advanceTimersByTimeAsync(0)
    expect(h.snapshots).toHaveLength(0)
    expect(h.state().phase).toBe('stopped')
    expect(vi.getTimerCount()).toBe(0)
    h.poller.resume()
    h.poller.refresh()
    expect(h.calls).toHaveLength(1)
  })

  it('stop after a success leaves no timers', async () => {
    const h = harness([snapshot()])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    h.poller.stop()
    expect(vi.getTimerCount()).toBe(0)
  })
})

describe('failures', () => {
  const network = new TypeError('Failed to fetch')

  it('backs off exponentially from the interval to a cap, and recovers on the first success', async () => {
    const h = harness([network, network, network, network, network, network, network, snapshot(2000)])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.state().failure).toMatchObject({ kind: 'network', attempts: 1, retryInMs: 2000 })
    await vi.advanceTimersByTimeAsync(2000)
    expect(h.state().failure).toMatchObject({ attempts: 2, retryInMs: 4000 })
    await vi.advanceTimersByTimeAsync(4000)
    expect(h.state().failure).toMatchObject({ attempts: 3, retryInMs: 8000 })
    await vi.advanceTimersByTimeAsync(8000)
    expect(h.state().failure).toMatchObject({ attempts: 4, retryInMs: 16_000 })
    await vi.advanceTimersByTimeAsync(16_000)
    expect(h.state().failure).toMatchObject({ attempts: 5, retryInMs: BACKOFF_CAP_MS })
    await vi.advanceTimersByTimeAsync(BACKOFF_CAP_MS)
    expect(h.state().failure).toMatchObject({ attempts: 6, retryInMs: BACKOFF_CAP_MS })
    await vi.advanceTimersByTimeAsync(BACKOFF_CAP_MS)
    expect(h.state().failure).toMatchObject({ attempts: 7, retryInMs: BACKOFF_CAP_MS })
    await vi.advanceTimersByTimeAsync(BACKOFF_CAP_MS)
    expect(h.state().failure).toBeUndefined()
    expect(h.snapshots).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(2000)
    expect(h.calls).toHaveLength(9)
    h.poller.stop()
  })

  it('jitters the delay by up to 10 percent', async () => {
    const low = harness([network], { random: () => 0 })
    low.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(low.state().failure?.retryInMs).toBe(1800)
    low.poller.stop()
    const high = harness([network], { random: () => 1 })
    high.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(high.state().failure?.retryInMs).toBe(2200)
    high.poller.stop()
  })

  it('names the reason: unavailable (503 stats_unavailable), http, decode and network', async () => {
    const kinds: string[] = []
    for (const error of [new StatsUnavailableError('sampling failed'), new HttpStatusError(404), new DecodeError('pools', 'expected a list'), network]) {
      const h = harness([error])
      h.poller.start()
      await vi.advanceTimersByTimeAsync(0)
      kinds.push(h.state().failure!.kind)
      h.poller.stop()
    }
    expect(kinds).toEqual(['unavailable', 'http', 'decode', 'network'])
  })

  it('keeps retrying after an unavailable answer and recovers', async () => {
    const h = harness([snapshot(), new StatsUnavailableError('x'), snapshot()])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(2000)
    expect(h.state().failure?.kind).toBe('unavailable')
    await vi.advanceTimersByTimeAsync(2000)
    expect(h.state().failure).toBeUndefined()
    expect(h.snapshots).toHaveLength(2)
    h.poller.stop()
  })

  it('stops for good on an unsupported schema version', async () => {
    const h = harness([new UnsupportedSchemaError(2)])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.state()).toMatchObject({ phase: 'stopped', failure: { kind: 'schema' } })
    await vi.advanceTimersByTimeAsync(120_000)
    expect(h.calls).toHaveLength(1)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not report an aborted request as a failure', async () => {
    const h = harness([() => Promise.reject(new DOMException('aborted', 'AbortError'))])
    h.poller.start()
    h.poller.pause()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.state().failure).toBeUndefined()
  })
})

describe('stale detection', () => {
  const network = new TypeError('Failed to fetch')

  it('flags data that stopped refreshing after three intervals and clears on the next success', async () => {
    const h = harness([snapshot(2000), network, network, network, snapshot(2000)])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(h.state().stale).toBe(false)
    expect(h.state().lastSuccessAtMs).toBe(1_000_000)
    await vi.advanceTimersByTimeAsync(STALE_FLOOR_MS - 1)
    expect(h.state().stale).toBe(false)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.state().stale).toBe(true)
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.state().stale).toBe(false)
    expect(h.state().lastSuccessAtMs).toBeGreaterThan(1_000_000)
    h.poller.stop()
  })

  it('scales the limit with long intervals', async () => {
    const h = harness([snapshot(20_000), network])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(59_999)
    expect(h.state().stale).toBe(false)
    await vi.advanceTimersByTimeAsync(1)
    expect(h.state().stale).toBe(true)
    h.poller.stop()
  })

  it('has nothing to call stale before the first snapshot', async () => {
    const h = harness([network])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.state().stale).toBe(false)
    expect(h.state().lastSuccessAtMs).toBeUndefined()
    h.poller.stop()
  })

  it('does not age while paused but is stale again on resume if the data is old', async () => {
    const h = harness([snapshot(2000), network])
    h.poller.start()
    await vi.advanceTimersByTimeAsync(0)
    h.poller.pause()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(h.state().stale).toBe(false)
    h.poller.resume()
    expect(h.state().stale).toBe(true)
    h.poller.stop()
  })
})

describe('fetchSnapshot', () => {
  const respond = (body: unknown, status = 200): typeof fetch => async () => new Response(JSON.stringify(body), { status })
  const controller = new AbortController()

  it('requests no-store with the abort signal and decodes the document', async () => {
    const fetcher = vi.fn<typeof fetch>(respond(wireDocument()))
    const result = await fetchSnapshot('/haproxy/snapshot.json', controller.signal, fetcher)
    expect(result.pools[0]!.servers[0]!.traffic.sessions.total).toBe(1000n)
    expect(fetcher).toHaveBeenCalledWith('/haproxy/snapshot.json', {
      cache: 'no-store', credentials: 'same-origin', headers: { Accept: 'application/json' }, signal: controller.signal,
    })
  })

  it('turns 503 stats_unavailable into StatsUnavailableError with the server message', async () => {
    const fetcher = respond({ error: { code: 'stats_unavailable', message: 'runtime statistics could not be sampled' } }, 503)
    await expect(fetchSnapshot('/s', controller.signal, fetcher)).rejects.toThrow(StatsUnavailableError)
    await expect(fetchSnapshot('/s', controller.signal, fetcher)).rejects.toThrow('runtime statistics could not be sampled')
  })

  it.each([[503, { error: { code: 'other', message: 'x' } }], [404, {}], [500, 'text']])('reports status %s as HttpStatusError', async (status, body) => {
    await expect(fetchSnapshot('/s', controller.signal, respond(body, status))).rejects.toMatchObject({ status })
  })

  it('lets network errors through unchanged', async () => {
    const failing: typeof fetch = async () => { throw new TypeError('Failed to fetch') }
    await expect(fetchSnapshot('/s', controller.signal, failing)).rejects.toThrow(TypeError)
  })

  it('rejects a document of another schema version', async () => {
    await expect(fetchSnapshot('/s', controller.signal, respond({ schemaVersion: 9 }))).rejects.toThrow(UnsupportedSchemaError)
  })
})

describe('snapshotUrl', () => {
  it.each([['/haproxy', '/haproxy/snapshot.json'], ['/haproxy/', '/haproxy/snapshot.json'], ['/', '/snapshot.json'],
    ['/a/b//', '/a/b/snapshot.json'], ['', '/snapshot.json']])('%s -> %s', (path, url) => {
    expect(snapshotUrl(path)).toBe(url)
  })
})
