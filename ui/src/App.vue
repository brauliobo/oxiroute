<template lang="pug">
main.console-shell(:aria-busy="activeView !== 'configuration' && monitoring === null && refreshing")
  header.masthead
    .brand-block
      p.eyebrow Network control / telemetry
      h1 OxiRoute
      p.deck {{ viewTitles[activeView] }}
    .system-state(v-if="activeView !== 'configuration'" :class="{ alert: monitoringError && !monitoring, stale: isStale }" role="status" aria-live="polite")
      span.state-light(aria-hidden="true")
      span {{ monitoringStatus }}
    .system-state.configuration-state(v-else role="status")
      span.state-light(aria-hidden="true")
      span Revision-aware editor

  nav.app-navigation(aria-label="Primary navigation")
    a(
      href="#/overview"
      :aria-current="activeView === 'overview' ? 'page' : undefined"
      @click="activateView('overview')"
    ) Overview
    a(
      href="#/stats"
      :aria-current="activeView === 'stats' ? 'page' : undefined"
      @click="activateView('stats')"
    ) Statistics
    a(
      href="#/operations"
      :aria-current="activeView === 'operations' ? 'page' : undefined"
      @click="activateView('operations')"
    ) Operations
    a(
      href="#/certificates"
      :aria-current="activeView === 'certificates' ? 'page' : undefined"
      @click="activateView('certificates')"
    ) Certificates
    a(
      href="#/events"
      :aria-current="activeView === 'events' ? 'page' : undefined"
      @click="activateView('events')"
    ) Events
    a(
      href="#/audit"
      :aria-current="activeView === 'audit' ? 'page' : undefined"
      @click="activateView('audit')"
    ) Audit
    a(
      href="#/configuration"
      :aria-current="activeView === 'configuration' ? 'page' : undefined"
      @click="activateView('configuration')"
    ) Configuration
    a(
      href="#/provenance"
      :aria-current="activeView === 'provenance' ? 'page' : undefined"
      @click="activateView('provenance')"
    ) Provenance
    form.management-auth(@submit.prevent="setManagementToken")
      label(for="management-access-token") Management token
      input#management-access-token(
        v-model="managementTokenInput"
        type="password"
        autocomplete="off"
        spellcheck="false"
        placeholder="Bearer token"
      )
      button(type="submit" :disabled="managementTokenInput.length === 0")
        | {{ managementToken ? 'Replace token' : 'Unlock telemetry' }}

  section.readout-bar(v-show="activeView !== 'configuration'" aria-label="Live monitoring summary")
    .readout
      span.label Active connections
      strong {{ monitoring ? formatCount(monitoring.traffic.activeConnections) : '--' }}
    .readout
      span.label Traffic moved
      strong {{ monitoring ? formatBytes(totalTrafficBytes(monitoring.traffic)) : '--' }}
    .readout
      span.label Host memory used
      strong {{ monitoring ? formatOptionalPercent(memoryUsagePercent(monitoring.host)) : '--' }}
    .readout
      span.label Uptime
      strong.mono {{ monitoring ? formatTelemetryDuration(monitoring.uptimeMs) : '--' }}
    button.refresh-button(type="button" @click="refresh" :disabled="refreshing")
      | {{ refreshing ? 'Refreshing...' : 'Refresh now' }}

  section.loading-state(v-if="activeView !== 'configuration' && !monitoring && refreshing" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Establishing telemetry
      p Waiting for the first monitoring sample from the control plane.

  p.notice.error-notice(v-if="activeView !== 'configuration' && monitoringError && !monitoring" role="alert")
    strong Monitoring unavailable.
    |  {{ monitoringError }}
  p.notice.stale-notice(v-else-if="activeView !== 'configuration' && monitoring && isStale" role="status" aria-live="polite")
    strong Retaining the last valid sample.
    |  {{ staleMessage }}
  p.notice.error-notice(v-if="activeView === 'overview' && catalogError" role="alert")
    strong {{ catalog ? 'Stream inventory refresh failed.' : 'Stream inventory unavailable.' }}
    |  {{ catalogError }}{{ catalog ? ' The last valid inventory remains visible.' : '' }}
  p.notice.loading-notice(v-else-if="activeView === 'overview' && !catalog && refreshing" role="status")
    strong Loading stream inventory.
    |  Waiting for the RTMP catalog.
  p.notice.error-notice(v-if="activeView === 'overview' && rtmpStatsError" role="alert")
    strong RTMP client control unavailable.
    |  {{ rtmpStatsError }}
  p.notice.error-notice(v-if="activeView === 'overview' && topologyError" role="alert")
    strong {{ topology ? 'Topology refresh failed.' : 'Topology unavailable.' }}
    |  {{ topologyError }}{{ topology ? ' The last valid topology remains visible.' : '' }}
  p.notice.loading-notice(v-else-if="activeView === 'overview' && !topology && refreshing" role="status")
    strong Loading topology.
    |  Waiting for the active runtime graph.
  p.notice.capability-notice(v-if="activeView === 'overview' && catalog && !catalog.capabilities.live_ingest")
    strong RTMP ingestion is not connected.
    |  Configure an RTMP listener to accept publishers; handshakes never create synthetic streams.
  p.notice.capability-notice(v-if="activeView === 'overview' && catalog && !catalog.capabilities.manual_recording")
    strong Manual recording controls are unavailable.
    |  Continuous recorder state remains visible, but this runtime does not accept start or stop commands.

  HaproxyStatsDashboard(v-if="activeView === 'stats' && monitoring" :monitoring="monitoring")

  OperationsWorkspace(v-if="activeView === 'operations'" :token="managementToken" @unauthorized="clearManagementToken")

  EventsWorkspace(v-if="activeView === 'events'" :token="managementToken" @unauthorized="clearManagementToken")

  AuditWorkspace(v-if="activeView === 'audit'" :token="managementToken" @unauthorized="clearManagementToken")

  CertificatesWorkspace(v-if="activeView === 'certificates'" :token="managementToken" @unauthorized="clearManagementToken")

  ProvenanceWorkspace(v-if="activeView === 'provenance'" :token="managementToken" @unauthorized="clearManagementToken")

  TopologyView(v-if="activeView === 'overview' && topology" :topology="topology")

  MonitoringOverview(v-if="activeView === 'overview' && monitoring" :monitoring="monitoring" :current-unix-ms="currentUnixMs")

  StreamInventory(
    :catalog="catalog"
    :current-unix-ms="currentUnixMs"
    :busy-recorder-id="busyRecorder"
    :visible="activeView === 'overview'"
    @control="controlRecorder"
  )

  RtmpClientPanel(
    v-if="activeView === 'overview'"
    :stats="rtmpStats"
    :busy-session-id="busySessionId"
    @drop="dropRtmpClient"
  )

  KeepAlive
    ConfigurationWorkspace(v-if="activeView === 'configuration'")

  footer.page-footer
    span OxiRoute pre-alpha
    span.mono Live measurements / runtime inventory
</template>

<script setup lang="ts" vapor>
import { computed, onMounted, onUnmounted, ref } from 'vue'

import ConfigurationWorkspace from './ConfigurationWorkspace.vue'
import AuditWorkspace from './AuditWorkspace.vue'
import CertificatesWorkspace from './CertificatesWorkspace.vue'
import EventsWorkspace from './EventsWorkspace.vue'
import HaproxyStatsDashboard from './HaproxyStatsDashboard.vue'
import { formatBytes, formatCount, formatTelemetryDuration, presentApiError } from './formatters'
import MonitoringOverview from './overview/MonitoringOverview.vue'
import StreamInventory from './overview/StreamInventory.vue'
import { formatOptionalPercent, formatSampleAge, memoryUsagePercent, totalTrafficBytes } from './overview/telemetry'
import RtmpClientPanel from './RtmpClientPanel.vue'
import { recorderControlAction } from './recording'
import TopologyView from './TopologyView.vue'
import OperationsWorkspace from './OperationsWorkspace.vue'
import ProvenanceWorkspace from './ProvenanceWorkspace.vue'
import {
  ApiError,
  connectEventStream,
  dropRtmpClient as requestRtmpClientDrop,
  fetchMonitoring,
  fetchRtmpCatalog,
  fetchRtmpStats,
  fetchTopology,
  setRecording,
  type EventStreamClient,
  type MonitoringSnapshot,
  type RecorderSnapshot,
  type RtmpCatalog,
  type RtmpClientControlTarget,
  type RtmpClientSnapshot,
  type RtmpStats,
  type StreamSnapshot,
  type TopologySnapshot,
} from './api'

const REFRESH_INTERVAL_MS = 5_000
const STALE_AFTER_MS = REFRESH_INTERVAL_MS * 3

type AppView = 'overview' | 'stats' | 'operations' | 'certificates' | 'events' | 'audit' | 'configuration' | 'provenance'

const viewTitles: Record<AppView, string> = {
  overview: 'Runtime observatory',
  stats: 'HAProxy statistics',
  operations: 'Local operations',
  certificates: 'Certificate inventory',
  events: 'Operational event history',
  audit: 'Durable audit history',
  configuration: 'Canonical configuration',
  provenance: 'Source provenance',
}

const activeView = ref<AppView>(viewFromHash())
const managementToken = ref('')
const managementTokenInput = ref('')
const monitoring = ref<MonitoringSnapshot | null>(null)
const catalog = ref<RtmpCatalog | null>(null)
const rtmpStats = ref<RtmpStats | null>(null)
const topology = ref<TopologySnapshot | null>(null)
const monitoringError = ref<string | null>(null)
const catalogError = ref<string | null>(null)
const rtmpStatsError = ref<string | null>(null)
const topologyError = ref<string | null>(null)
const refreshing = ref(true)
const busyRecorder = ref<string | null>(null)
const busySessionId = ref<string | null>(null)
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
const monitoringStatus = computed(() => {
  if (!monitoring.value) return refreshing.value ? 'Establishing telemetry' : 'Telemetry offline'
  if (isStale.value) return 'Telemetry stale'
  return refreshing.value ? 'Synchronizing' : 'Telemetry live'
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

function viewFromHash(): AppView {
  if (window.location.hash === '#/configuration') return 'configuration'
  if (window.location.hash === '#/stats') return 'stats'
  if (window.location.hash === '#/operations') return 'operations'
  if (window.location.hash === '#/certificates') return 'certificates'
  if (window.location.hash === '#/events') return 'events'
  if (window.location.hash === '#/audit') return 'audit'
  return window.location.hash === '#/provenance' ? 'provenance' : 'overview'
}

function activateView(view: AppView): void {
  activeView.value = view
  if (view !== 'configuration') {
    startMonitoring()
    if (view !== 'overview' && view !== 'stats') stopEventStream()
  } else {
    stopMonitoring()
  }
}

function setManagementToken(): void {
  if (!managementTokenInput.value) return
  managementToken.value = managementTokenInput.value
  managementTokenInput.value = ''
  if (activeView.value !== 'configuration') {
    stopEventStream()
    if (activeView.value === 'overview' || activeView.value === 'stats') startEventStream()
    void refresh()
  }
}

function clearManagementToken(): void {
  managementToken.value = ''
  managementTokenInput.value = ''
  monitoringError.value = 'Management token rejected. Enter a valid bearer token.'
  stopEventStream()
}

function syncViewFromHash(): void {
  activateView(viewFromHash())
}

function startMonitoring(): void {
  if (monitoringStarted) {
    if (activeView.value === 'overview' || activeView.value === 'stats') startEventStream()
    return
  }
  monitoringStarted = true
  if (activeView.value === 'overview' || activeView.value === 'stats') startEventStream()
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
  if (eventStream || !managementToken.value || !monitoringStarted) return
  const client = connectEventStream(managementToken.value, {
    onEvent: () => {
      void refresh()
    },
    onResyncRequired: async () => {
      await refresh()
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

async function refreshData(controller: AbortController): Promise<void> {
  const requests = [refreshMonitoring(controller)]
  if (activeView.value === 'overview') {
    requests.push(refreshCatalog(controller), refreshRtmpStats(controller), refreshTopology(controller))
  }
  await Promise.all(requests)
}

async function refreshMonitoring(controller: AbortController): Promise<void> {
  try {
    const result = await fetchMonitoring(controller.signal, managementToken.value || undefined)
    if (activeController !== controller) return
    monitoring.value = result
    monitoringError.value = null
  } catch (error) {
    if (activeController === controller) {
      monitoringError.value = presentApiError(error, 'Unable to load monitoring telemetry')
    }
  }
}

async function refreshCatalog(controller: AbortController): Promise<void> {
  try {
    const result = await fetchRtmpCatalog(controller.signal, managementToken.value || undefined)
    if (activeController !== controller) return
    catalog.value = result
    catalogError.value = null
  } catch (error) {
    if (activeController === controller) catalogError.value = presentApiError(error, 'Unable to load RTMP state')
  }
}

async function refreshRtmpStats(controller: AbortController): Promise<void> {
  try {
    const result = await fetchRtmpStats(controller.signal, managementToken.value || undefined)
    if (activeController !== controller) return
    rtmpStats.value = result
    rtmpStatsError.value = null
  } catch (error) {
    if (activeController === controller) rtmpStatsError.value = presentApiError(error, 'Unable to load RTMP client state')
  }
}

async function refreshTopology(controller: AbortController): Promise<void> {
  try {
    const result = await fetchTopology(controller.signal, managementToken.value || undefined)
    if (activeController !== controller) return
    topology.value = result
    topologyError.value = null
  } catch (error) {
    if (activeController === controller) topologyError.value = presentApiError(error, 'Unable to load active topology')
  }
}

async function controlRecorder(
  stream: StreamSnapshot,
  recorder: RecorderSnapshot,
): Promise<void> {
  const action = recorderControlAction(
    catalog.value?.capabilities.manual_recording ?? false,
    stream,
    recorder,
  )
  if (!action) {
    catalogError.value = 'Recorder state changed before the command could be sent. Refreshing the active stream state.'
    await refreshAfterRecorderCommand()
    return
  }
  busyRecorder.value = recorder.id
  catalogError.value = null
  try {
    const result = await setRecording(stream.id, recorder.id, action, managementToken.value || undefined)
    if (result.id !== recorder.id) {
      throw new Error('Recorder command returned a mismatched recorder identity.')
    }
    if (!catalog.value?.streams.some(({ id }) => id === stream.id)) {
      await refreshAfterRecorderCommand()
      catalogError.value = staleRecorderMessage(stream)
      return
    }
    await refreshAfterRecorderCommand()
    if (!catalog.value?.streams.some(({ id }) => id === stream.id)) {
      catalogError.value = staleRecorderMessage(stream)
    } else if (!catalog.value.streams
      .find(({ id }) => id === stream.id)
      ?.recorders.some(({ id }) => id === recorder.id)
    ) {
      catalogError.value = 'The recorder no longer exists on the active stream. No command success was assumed.'
    }
  } catch (requestError) {
    const controlError = recorderControlError(requestError)
    if (requestError instanceof ApiError && [404, 409].includes(requestError.status)) {
      await refreshAfterRecorderCommand()
    }
    catalogError.value = controlError
  } finally {
    if (busyRecorder.value === recorder.id) busyRecorder.value = null
  }
}

async function dropRtmpClient(
  client: RtmpClientSnapshot,
  target: RtmpClientControlTarget,
): Promise<void> {
  if (!managementToken.value) {
    rtmpStatsError.value = 'Enter the management token before changing RTMP client state.'
    return
  }
  busySessionId.value = client.id
  rtmpStatsError.value = null
  try {
    const result = await requestRtmpClientDrop(client, target, managementToken.value)
    if (result.sessionId !== client.id || result.target !== target) {
      throw new Error('RTMP control returned a mismatched session identity.')
    }
    await refreshAfterClientCommand()
  } catch (requestError) {
    if (requestError instanceof ApiError && [404, 409].includes(requestError.status)) {
      await refreshAfterClientCommand()
    }
    rtmpStatsError.value = rtmpControlError(requestError)
  } finally {
    if (busySessionId.value === client.id) busySessionId.value = null
  }
}

async function refreshAfterClientCommand(): Promise<void> {
  if (activeRefresh) await activeRefresh
  await refresh()
}

function rtmpControlError(error: unknown): string {
  if (!(error instanceof ApiError)) return presentApiError(error, 'RTMP client control failed')
  switch (error.code) {
    case 'session_not_found':
      return 'The RTMP client ended before the control request was applied.'
    case 'session_revision_conflict':
    case 'session_role_conflict':
      return 'The RTMP client state changed before the control request was applied. Refreshing the current state.'
    case 'session_control_pending':
      return 'A control request is already pending for this RTMP client.'
    default:
      return `RTMP client control failed. ${error.message}`
  }
}

async function refreshAfterRecorderCommand(): Promise<void> {
  if (activeRefresh) await activeRefresh
  await refresh()
}

function staleRecorderMessage(stream: StreamSnapshot): string {
  const replacement = catalog.value?.streams.some((candidate) =>
    candidate.server_id === stream.server_id &&
    candidate.application === stream.application &&
    candidate.name === stream.name,
  )
  return replacement
    ? 'The publisher stream was replaced while the recorder command was in flight. The replacement state is shown; no command success was assumed.'
    : 'The publisher stream ended while the recorder command was in flight. No command success was assumed.'
}

function recorderControlError(error: unknown): string {
  if (!(error instanceof ApiError)) return presentApiError(error, 'Recorder command failed')
  switch (error.code) {
    case 'rtmp_recording_unavailable':
      return 'Manual recording is unavailable in the active runtime.'
    case 'rtmp_recorder_start_failed':
      return 'Recorder command failed. The recorder could not be started.'
    case 'rtmp_recorder_stop_failed':
      return 'Recorder command failed. The recorder could not be stopped.'
    case 'rtmp_resource_not_found':
      return 'The target stream or recorder no longer exists. Active stream state was refreshed.'
    case 'rtmp_state_conflict':
      return `The recorder state changed before the command completed. ${error.message}`
    default:
      return `Recorder command failed. ${error.message}`
  }
}

onMounted(() => {
  window.addEventListener('hashchange', syncViewFromHash)
  if (activeView.value !== 'configuration') startMonitoring()
})

onUnmounted(() => {
  window.removeEventListener('hashchange', syncViewFromHash)
  stopMonitoring()
})
</script>

<style scoped src="./consoleBase.css"></style>
<style scoped src="./App.css"></style>
