import { computed, onUnmounted, ref, watch } from 'vue'
import type { Ref } from 'vue'

import {
  ApiError,
  connectEventStream,
  fetchMonitoring,
  fetchRtmpCatalog,
  fetchRtmpStats,
  fetchTopology,
} from '../api'
import type {
  EventStreamClient,
  MonitoringSnapshot,
  RtmpCatalog,
  RtmpStats,
  TopologySnapshot,
} from '../api'
import { presentApiError } from '../formatters'
import { formatSampleAge } from '../overview/telemetry'
import type { FeedState } from './feed'
import type { ViewId } from './views'

const REFRESH_INTERVAL_MS = 5_000
const STALE_AFTER_MS = REFRESH_INTERVAL_MS * 3

interface FeedOptions {
  token: Ref<string>
  view: Ref<ViewId>
  onUnauthorized: () => void
}

const streamsEvents = (view: ViewId) => view === 'overview' || view === 'stats' || view === 'cameras'
const readsRtmp = (view: ViewId) => view === 'overview' || view === 'cameras'

export function useTelemetryFeed({ token, view, onUnauthorized }: FeedOptions) {
  const monitoring = ref<MonitoringSnapshot | null>(null)
  const catalog = ref<RtmpCatalog | null>(null)
  const rtmpStats = ref<RtmpStats | null>(null)
  const topology = ref<TopologySnapshot | null>(null)
  const monitoringError = ref<string | null>(null)
  const catalogError = ref<string | null>(null)
  const rtmpStatsError = ref<string | null>(null)
  const topologyError = ref<string | null>(null)
  const refreshing = ref(false)
  const currentUnixMs = ref(Date.now())
  let refreshTimer: number | undefined
  let activeController: AbortController | undefined
  let activeRefresh: Promise<void> | null = null
  let eventStream: EventStreamClient | null = null
  let monitoringStarted = false

  const isStale = computed(
    () =>
      monitoring.value !== null &&
      (monitoringError.value !== null ||
        currentUnixMs.value - monitoring.value.sampledAtUnixMs > STALE_AFTER_MS),
  )
  const feedState = computed<FeedState>(() => {
    if (view.value === 'configuration') return 'paused'
    if (!monitoring.value) return refreshing.value ? 'loading' : 'unavailable'
    return isStale.value ? 'stale' : 'live'
  })
  const staleMessage = computed(() => {
    if (monitoringError.value) return `${monitoringError.value} The displayed values have not been cleared.`
    if (!monitoring.value) return ''
    return `The latest sample is ${formatSampleAge(currentUnixMs.value, monitoring.value.sampledAtUnixMs)}.`
  })

  function refresh(): Promise<void> {
    currentUnixMs.value = Date.now()
    if (activeRefresh) return activeRefresh

    const controller = new AbortController()
    activeController = controller
    refreshing.value = true

    const refreshPromise = refreshData(controller).finally(() => {
      if (activeController !== controller) return
      activeController = undefined
      currentUnixMs.value = Date.now()
      refreshing.value = false
      if (activeRefresh === refreshPromise) activeRefresh = null
    })
    activeRefresh = refreshPromise
    return activeRefresh
  }

  async function refreshAfterCommand(): Promise<void> {
    if (activeRefresh) await activeRefresh
    await refresh()
  }

  async function refreshData(controller: AbortController): Promise<void> {
    const requests = [load(controller, fetchMonitoring, monitoring, monitoringError, 'Unable to load monitoring telemetry')]
    if (readsRtmp(view.value)) {
      requests.push(
        load(controller, fetchRtmpCatalog, catalog, catalogError, 'Unable to load RTMP state'),
        load(controller, fetchRtmpStats, rtmpStats, rtmpStatsError, 'Unable to load RTMP client state'),
      )
    }
    if (view.value === 'overview') {
      requests.push(load(controller, fetchTopology, topology, topologyError, 'Unable to load active topology'))
    }
    await Promise.all(requests)
  }

  async function load<T>(
    controller: AbortController,
    fetcher: (token: string, signal: AbortSignal) => Promise<T>,
    target: Ref<T | null>,
    error: Ref<string | null>,
    fallback: string,
  ): Promise<void> {
    try {
      const result = await fetcher(token.value, controller.signal)
      if (activeController !== controller) return
      target.value = result
      error.value = null
    } catch (failure) {
      if (activeController !== controller) return
      if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
      else error.value = presentApiError(failure, fallback)
    }
  }

  function startMonitoring(): void {
    if (monitoringStarted) return
    monitoringStarted = true
    void refresh()
    refreshTimer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS)
  }

  function stopMonitoring(): void {
    stopEventStream()
    if (refreshTimer !== undefined) window.clearInterval(refreshTimer)
    refreshTimer = undefined
    monitoringStarted = false
    activeController?.abort()
    activeController = undefined
    activeRefresh = null
    refreshing.value = false
  }

  function startEventStream(): void {
    if (eventStream) return
    const client = connectEventStream(token.value, {
      onEvent: () => void refresh(),
      onResyncRequired: async () => {
        await refresh()
      },
      onError: (failure) => {
        if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
      },
    })
    eventStream = client
    void client.closed.then(() => {
      if (eventStream === client) eventStream = null
    })
  }

  function stopEventStream(): void {
    eventStream?.close()
    eventStream = null
  }

  function forget(): void {
    monitoring.value = null
    catalog.value = null
    rtmpStats.value = null
    topology.value = null
    monitoringError.value = null
    catalogError.value = null
    rtmpStatsError.value = null
    topologyError.value = null
  }

  watch([token, view], ([nextToken, nextView]) => {
    if (nextToken === '') {
      stopMonitoring()
      forget()
      return
    }
    if (nextView === 'configuration') {
      stopMonitoring()
      return
    }
    const viewChanged = monitoringStarted
    startMonitoring()
    if (viewChanged) void refresh()
    if (streamsEvents(nextView)) startEventStream()
    else stopEventStream()
  }, { immediate: true })

  onUnmounted(stopMonitoring)

  return {
    monitoring,
    catalog,
    rtmpStats,
    topology,
    monitoringError,
    catalogError,
    rtmpStatsError,
    topologyError,
    refreshing,
    currentUnixMs,
    isStale,
    feedState,
    staleMessage,
    refresh,
    refreshAfterCommand,
  }
}
