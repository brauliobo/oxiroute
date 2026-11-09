import { computed, shallowRef } from 'vue'
import type { ComputedRef, ShallowRef } from 'vue'
import type { StatsSnapshot } from './decode'
import { deriveModel } from './derive'
import type { MeasuredModel } from './derive'
import { buildModel, modelCounters, modelGauges } from './model'
import { createPoller, DEFAULT_INTERVAL_MS, fetchSnapshot, snapshotUrl } from './poll'
import type { PollFailure, PollState, Poller } from './poll'
import { queryListeners, queryPools } from './query'
import { DEFAULT_MAX_GAP_MS, createRateTracker } from './rates'
import type { RateOutcome } from './rates'
import { SeriesBook } from './series'
import { MAX_KEYS, defaultView, parseViewState, replaceUrlState } from './urlState'
import type { ViewState } from './urlState'

export type StoreStatus = 'loading' | 'live' | 'paused' | 'stale' | 'retrying' | 'error' | 'unsupported'
type UrlWindow = Parameters<typeof replaceUrlState>[1]

export interface StatsStoreOptions {
  /** Defaults to `<page path>/snapshot.json` of the current page. */
  load?(signal: AbortSignal): Promise<StatsSnapshot>
  /** The address bar the view state lives in; defaults to `window`. */
  location?: UrlWindow
  historyCapacity?: number
  /** How often `nowMs` (and so `ageMs`) advances. */
  tickMs?: number
  random?(): number
}

export interface StatsStore {
  /** The latest decoded snapshot; kept while stale or failing, absent only before the first one. */
  readonly snapshot: ShallowRef<StatsSnapshot | undefined>
  /** The latest snapshot as rows with stable keys, rates and percentiles. */
  readonly model: ShallowRef<MeasuredModel | undefined>
  /** The model rows narrowed and ordered by the view. */
  readonly visible: ComputedRef<Pick<MeasuredModel, 'listeners' | 'pools'> | undefined>
  readonly view: Readonly<ShallowRef<ViewState>>
  readonly poll: Readonly<ShallowRef<PollState>>
  readonly status: ComputedRef<StoreStatus>
  readonly failure: ComputedRef<PollFailure | undefined>
  /** The interval in force: the viewer's, else the server's hint. */
  readonly intervalMs: ComputedRef<number>
  /** Time of the last good snapshot on the browser clock; for "updated Ns ago". */
  readonly lastUpdatedAtMs: ComputedRef<number | undefined>
  readonly nowMs: Readonly<ShallowRef<number>>
  readonly ageMs: ComputedRef<number | undefined>
  /** What the last snapshot did to the rates: `reset` means one interval without them after a reload or restart. */
  readonly rateOutcome: Readonly<ShallowRef<RateOutcome | undefined>>
  /** History of `<row key>#<metric>` (rates: requests, errors, bytesReceived, bytesSent, accepted) and `#active`. */
  series(key: string): Array<number | undefined>
  /** Server sample times of every series point, oldest first. */
  seriesTimes(): number[]
  setView(patch: Partial<ViewState>): void
  toggleExpanded(key: string): void
  toggleCollapsed(key: string): void
  setPaused(paused: boolean): void
  togglePause(): void
  setIntervalMs(intervalMs: number | undefined): void
  refresh(): void
  start(): void
  /** Aborts the request in flight and cancels every timer. */
  dispose(): void
}

const toggled = (keys: string[], key: string) =>
  (keys.includes(key) ? keys.filter((item) => item !== key) : [...keys, key]).slice(-MAX_KEYS)

export function createStatsStore(options: StatsStoreOptions = {}): StatsStore {
  const urlWindow: UrlWindow = options.location ?? window
  const view = shallowRef<ViewState>(urlWindow.location.search ? parseViewState(urlWindow.location.search) : defaultView())
  const snapshot = shallowRef<StatsSnapshot>()
  const model = shallowRef<MeasuredModel>()
  const pollState = shallowRef<PollState>({ phase: 'idle', loading: false, stale: false })
  const rateOutcome = shallowRef<RateOutcome>()
  const nowMs = shallowRef(Date.now())
  const version = shallowRef(0)
  const book = new SeriesBook(options.historyCapacity)
  const tracker = createRateTracker()
  let rates: ReadonlyMap<string, number> = new Map()
  let ticker: ReturnType<typeof setInterval> | undefined

  const intervalMs = computed(() => view.value.intervalMs ?? snapshot.value?.intervalHintMs ?? DEFAULT_INTERVAL_MS)

  function accept(next: StatsSnapshot) {
    const base = buildModel(next)
    const maxGapMs = Math.max(DEFAULT_MAX_GAP_MS, 4 * intervalMs.value)
    const result = tracker.update({
      atMs: next.sampledAtUnixMs,
      generation: next.generation.number,
      uptimeMs: next.process.uptimeMs,
      counters: modelCounters(base),
    }, maxGapMs)
    if (result.outcome === 'same') return
    rateOutcome.value = result.outcome
    rates = result.rates
    book.record(next.sampledAtUnixMs, new Map([...modelGauges(base), ...rates]))
    version.value++
    snapshot.value = next
    model.value = deriveModel(base, rates)
  }

  const poller: Poller = createPoller({
    load: options.load ?? ((signal) => fetchSnapshot(snapshotUrl(urlWindow.location.pathname), signal)),
    onSnapshot: accept,
    onState: (state) => { pollState.value = state },
    intervalMs: view.value.intervalMs,
    paused: view.value.paused,
    random: options.random,
  })

  const failure = computed(() => pollState.value.failure)
  const status = computed<StoreStatus>(() => {
    const { phase, stale } = pollState.value
    if (failure.value?.kind === 'schema') return 'unsupported'
    if (!snapshot.value) return failure.value ? 'error' : 'loading'
    if (phase === 'paused') return 'paused'
    if (stale) return 'stale'
    return failure.value ? 'retrying' : 'live'
  })
  const visible = computed(() => model.value && {
    listeners: queryListeners(model.value.listeners, view.value),
    pools: queryPools(model.value.pools, view.value),
  })
  const lastUpdatedAtMs = computed(() => pollState.value.lastSuccessAtMs)
  const ageMs = computed(() => (lastUpdatedAtMs.value === undefined ? undefined : Math.max(0, nowMs.value - lastUpdatedAtMs.value)))

  function setView(patch: Partial<ViewState>) {
    view.value = { ...view.value, ...patch }
    replaceUrlState(view.value, urlWindow)
  }

  function setPaused(paused: boolean) {
    setView({ paused })
    if (paused) poller.pause()
    else poller.resume()
  }

  return {
    snapshot, model, visible, view, poll: pollState, status, failure, intervalMs, lastUpdatedAtMs, nowMs, ageMs, rateOutcome,
    series: (key) => (version.value, book.series(key)),
    seriesTimes: () => (version.value, book.times()),
    setView,
    toggleExpanded: (key) => setView({ expanded: toggled(view.value.expanded, key) }),
    toggleCollapsed: (key) => setView({ collapsed: toggled(view.value.collapsed, key) }),
    setPaused,
    togglePause: () => setPaused(!view.value.paused),
    setIntervalMs(next) {
      setView({ intervalMs: next })
      poller.setIntervalMs(next)
    },
    refresh: () => poller.refresh(),
    start() {
      ticker ??= setInterval(() => { nowMs.value = Date.now() }, options.tickMs ?? 1000)
      poller.start()
    },
    dispose() {
      clearInterval(ticker)
      poller.stop()
    },
  }
}
