import { DecodeError, UnsupportedSchemaError, decodeSnapshot } from './decode'
import type { StatsSnapshot } from './decode'

// Polls snapshot.json. The server rebuilds the snapshot once a second and states how often to ask (`intervalHintMs`),
// so the delay is the viewer's interval, else the hint of the latest snapshot, never below one second. One request is
// in flight at a time. Failures back off exponentially (interval, 2x, 4x ... up to 30 s) with +-10 % jitter and the
// poller keeps retrying, except for an unsupported schema version, which ends polling. Pausing aborts the request in
// flight and drops its answer; resuming polls at once. Data that did not refresh for three intervals (at least 10 s)
// is stale: the last snapshot is kept for display, marked, never replaced by zeros.

export const MIN_INTERVAL_MS = 1000
export const DEFAULT_INTERVAL_MS = 2000
export const BACKOFF_CAP_MS = 30_000
export const STALE_FLOOR_MS = 10_000
const STALE_INTERVALS = 3
const JITTER = 0.1

/** 503 `stats_unavailable`: the runtime could not be sampled right now; a later request may succeed. */
export class StatsUnavailableError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'StatsUnavailableError'
  }
}

export class HttpStatusError extends Error {
  readonly status: number

  constructor(status: number) {
    super(`unexpected HTTP status ${status}`)
    this.name = 'HttpStatusError'
    this.status = status
  }
}

/** `<page path>/snapshot.json`: the page is served at the configured prefix, with or without a trailing slash. */
export const snapshotUrl = (pathname: string) => `${pathname.replace(/\/+$/, '')}/snapshot.json`

export async function fetchSnapshot(url: string, signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<StatsSnapshot> {
  const response = await fetcher(url, { cache: 'no-store', credentials: 'same-origin', headers: { Accept: 'application/json' }, signal })
  if (response.status === 503) {
    const body = await response.json().catch(() => undefined)
    if (body?.error?.code === 'stats_unavailable') throw new StatsUnavailableError(body.error.message)
  }
  if (!response.ok) throw new HttpStatusError(response.status)
  return decodeSnapshot(await response.json())
}

export type FailureKind = 'unavailable' | 'network' | 'http' | 'decode' | 'schema'
export interface PollFailure {
  kind: FailureKind
  message: string
  /** Consecutive failures so far. */
  attempts: number
  /** Delay until the next request; absent when polling ended (`schema`). */
  retryInMs?: number
}

export interface PollState {
  phase: 'idle' | 'running' | 'paused' | 'stopped'
  loading: boolean
  stale: boolean
  lastSuccessAtMs?: number
  failure?: PollFailure
}

export interface PollerOptions {
  load(signal: AbortSignal): Promise<StatsSnapshot>
  onSnapshot(snapshot: StatsSnapshot, receivedAtMs: number): void
  onState?(state: PollState): void
  /** The viewer's interval; undefined follows the server's hint. */
  intervalMs?: number
  paused?: boolean
  now?(): number
  random?(): number
}

export interface Poller {
  start(): void
  pause(): void
  resume(): void
  /** Loads now, unless a request is in flight. */
  refresh(): void
  setIntervalMs(intervalMs: number | undefined): void
  /** Aborts the request in flight and cancels every timer; the poller cannot be restarted. */
  stop(): void
}

function classify(error: unknown): FailureKind {
  if (error instanceof UnsupportedSchemaError) return 'schema'
  if (error instanceof StatsUnavailableError) return 'unavailable'
  if (error instanceof HttpStatusError) return 'http'
  return error instanceof DecodeError ? 'decode' : 'network'
}

export function createPoller(options: PollerOptions): Poller {
  const now = options.now ?? Date.now
  const random = options.random ?? Math.random
  let state: PollState = { phase: 'idle', loading: false, stale: false }
  let intervalMs = options.intervalMs
  let hintMs = DEFAULT_INTERVAL_MS
  let attempts = 0
  let request: AbortController | undefined
  let pollTimer: ReturnType<typeof setTimeout> | undefined
  let staleTimer: ReturnType<typeof setTimeout> | undefined

  const baseMs = () => Math.max(MIN_INTERVAL_MS, intervalMs ?? hintMs)
  const active = () => state.phase === 'running'

  function update(patch: Partial<PollState>) {
    state = { ...state, ...patch }
    options.onState?.(state)
  }

  function schedule(delayMs: number) {
    clearTimeout(pollTimer)
    pollTimer = setTimeout(poll, delayMs)
  }

  // Stale is a property of running polls: a paused page shows the age of its data, it is not "behind".
  function armStale() {
    clearTimeout(staleTimer)
    if (!active() || state.lastSuccessAtMs === undefined) return update({ stale: false })
    const remaining = state.lastSuccessAtMs + Math.max(STALE_FLOOR_MS, STALE_INTERVALS * baseMs()) - now()
    update({ stale: remaining <= 0 })
    if (remaining > 0) staleTimer = setTimeout(armStale, remaining)
  }

  async function poll() {
    if (!active() || request) return
    clearTimeout(pollTimer)
    const own = (request = new AbortController())
    update({ loading: true })
    try {
      const snapshot = await options.load(own.signal)
      if (own.signal.aborted) return
      request = undefined
      attempts = 0
      hintMs = snapshot.intervalHintMs
      update({ loading: false, lastSuccessAtMs: now(), failure: undefined })
      options.onSnapshot(snapshot, state.lastSuccessAtMs!)
      armStale()
      schedule(baseMs())
    } catch (error) {
      if (own.signal.aborted) return
      request = undefined
      fail(error)
    }
  }

  function fail(error: unknown) {
    const kind = classify(error)
    attempts += 1
    const message = error instanceof Error ? error.message : String(error)
    if (kind === 'schema') {
      update({ phase: 'stopped', loading: false, stale: false, failure: { kind, message, attempts } })
      return stop()
    }
    const delay = Math.min(BACKOFF_CAP_MS, baseMs() * 2 ** (attempts - 1))
    const retryInMs = Math.round(delay * (1 - JITTER + 2 * JITTER * random()))
    update({ loading: false, failure: { kind, message, attempts, retryInMs } })
    schedule(retryInMs)
  }

  function halt() {
    request?.abort()
    request = undefined
    clearTimeout(pollTimer)
    clearTimeout(staleTimer)
  }

  function stop() {
    halt()
    if (state.phase !== 'stopped') update({ phase: 'stopped', loading: false, stale: false })
  }

  return {
    start() {
      if (state.phase !== 'idle') return
      update({ phase: options.paused ? 'paused' : 'running' })
      if (active()) void poll()
    },
    pause() {
      if (!active()) return
      halt()
      update({ phase: 'paused', loading: false, stale: false })
    },
    resume() {
      if (state.phase !== 'paused') return
      update({ phase: 'running' })
      armStale()
      void poll()
    },
    refresh() { void poll() },
    setIntervalMs(next) {
      intervalMs = next
      if (!active()) return
      armStale()
      if (!request) schedule(state.failure?.retryInMs === undefined ? baseMs() : state.failure.retryInMs)
    },
    stop,
  }
}
