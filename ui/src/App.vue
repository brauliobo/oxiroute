<template lang="pug">
main.console-shell(:aria-busy="monitoring === null && refreshing")
  header.masthead
    .brand-block
      p.eyebrow Network control / telemetry
      h1 OxiRoute
      p.deck Runtime observatory
    .system-state(:class="{ alert: monitoringError && !monitoring, stale: isStale }" role="status" aria-live="polite")
      span.state-light(aria-hidden="true")
      span {{ monitoringStatus }}

  section.readout-bar(aria-label="Live monitoring summary")
    .readout
      span.label Active connections
      strong {{ monitoring ? formatCount(monitoring.traffic.activeConnections) : '--' }}
    .readout
      span.label Traffic moved
      strong {{ monitoring ? formatBytes(totalTrafficBytes) : '--' }}
    .readout
      span.label Host memory used
      strong {{ monitoring ? formatPercent(memoryUsagePercent) : '--' }}
    .readout
      span.label Uptime
      strong.mono {{ monitoring ? formatDuration(monitoring.uptimeMs) : '--' }}
    button.refresh-button(type="button" @click="refresh" :disabled="refreshing")
      | {{ refreshing ? 'Refreshing...' : 'Refresh now' }}

  section.loading-state(v-if="!monitoring && refreshing" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Establishing telemetry
      p Waiting for the first monitoring sample from the control plane.

  p.notice.error-notice(v-if="monitoringError && !monitoring" role="alert")
    strong Monitoring unavailable.
    |  {{ monitoringError }}
  p.notice.stale-notice(v-else-if="monitoring && isStale" role="status" aria-live="polite")
    strong Retaining the last valid sample.
    |  {{ staleMessage }}
  p.notice.error-notice(v-if="catalogError" role="alert")
    strong Stream inventory unavailable.
    |  {{ catalogError }}
  p.notice.capability-notice(v-if="catalog && !catalog.capabilities.live_ingest")
    strong RTMP ingestion is not connected.
    |  Configure an RTMP listener to accept publishers; handshakes never create synthetic streams.

  section.monitoring-overview(v-if="monitoring" aria-labelledby="monitoring-heading")
    .section-heading.monitoring-heading
      div
        p.eyebrow Live infrastructure
        h2#monitoring-heading Monitoring overview
      p.snapshot-time Snapshot {{ formatTime(monitoring.sampledAtUnixMs) }} / {{ formatSampleAge(monitoring.sampledAtUnixMs) }}

    .monitor-grid
      article.monitor-panel.traffic-panel
        header.panel-heading
          div
            p.eyebrow Aggregate network
            h3 Traffic
          span.panel-index 01
        .traffic-primary
          div
            strong.metric-display {{ formatCount(monitoring.traffic.activeConnections) }}
            span.metric-caption Connections active now
          .connection-total
            span.label Lifetime accepted
            strong {{ formatCount(monitoring.traffic.acceptedConnections) }}
        .direction-grid
          .direction-card
            span.direction-arrow(aria-hidden="true") IN
            div
              span.label Inbound
              strong {{ formatBytes(monitoring.traffic.bytesReceived) }}
          .direction-card
            span.direction-arrow.outbound(aria-hidden="true") OUT
            div
              span.label Outbound
              strong {{ formatBytes(monitoring.traffic.bytesSent) }}

      article.monitor-panel.host-panel
        header.panel-heading
          div
            p.eyebrow Host pressure
            h3 Load / memory
          span.panel-index 02
        .load-list(aria-label="Host load averages")
          .load-row(v-for="load in hostLoads" :key="load.label")
            span.load-window {{ load.label }}
            .load-track(aria-hidden="true")
              span(:style="{ width: loadBarWidth(load.value) }")
            strong {{ formatDecimal(load.value) }}
        .memory-readout
          .memory-copy
            span.label Memory used
            strong {{ formatBytes(usedMemoryBytes) }} / {{ formatBytes(monitoring.host.totalMemoryBytes) }}
          .memory-track(aria-hidden="true")
            span(:style="{ width: `${memoryUsagePercent}%` }")
          span.available-copy {{ formatBytes(monitoring.host.availableMemoryBytes) }} available

      article.monitor-panel.process-panel
        header.panel-heading
          div
            p.eyebrow OxiRoute process
            h3 Runtime
          span.panel-index 03
        .process-primary
          span.label CPU utilization
          strong.metric-display {{ formatOptionalPercent(monitoring.process.cpuPercent) }}
        .process-grid
          .compact-metric
            span.label Resident
            strong {{ formatBytes(monitoring.process.residentMemoryBytes) }}
          .compact-metric
            span.label Virtual
            strong {{ formatBytes(monitoring.process.virtualMemoryBytes) }}
          .compact-metric
            span.label Threads
            strong {{ formatCount(monitoring.process.threadCount) }}
          .compact-metric
            span.label Open files
            strong {{ formatCount(monitoring.process.openFileDescriptors) }}

      article.monitor-panel.rtmp-panel
        header.panel-heading
          div
            p.eyebrow Media plane
            h3 RTMP pulse
          span.panel-index 04
        .rtmp-signal
          span.signal-ring(aria-hidden="true")
          div
            strong.metric-display {{ formatCount(monitoring.rtmp.activeStreams) }}
            span.metric-caption Active {{ monitoring.rtmp.activeStreams === 1 ? 'stream' : 'streams' }}
        .rtmp-grid
          .compact-metric
            span.label Publishers
            strong {{ formatCount(monitoring.rtmp.publishers) }}
          .compact-metric
            span.label Subscribers
            strong {{ formatCount(monitoring.rtmp.subscribers) }}
          .compact-metric.payload-metric
            span.label Active media
            strong {{ formatBytes(monitoring.rtmp.mediaPayloadBytesReceived) }}

    section.listener-section(aria-labelledby="listener-heading")
      .listener-heading
        div
          p.eyebrow Bound surfaces
          h3#listener-heading Listeners
        span.listener-count {{ monitoring.listeners.length }} {{ monitoring.listeners.length === 1 ? 'endpoint' : 'endpoints' }}
      p.listener-empty(v-if="monitoring.listeners.length === 0") No listeners are currently bound.
      .listener-list(v-else)
        article.listener-row(v-for="listener in monitoring.listeners" :key="`${listener.protocol}:${listener.name}:${listener.bind}`")
          header.listener-identity
            span.protocol-badge(:class="`protocol-${listener.protocol}`") {{ listener.protocol }}
            div
              h4 {{ listener.name }}
              code {{ listener.bind }}
          .listener-metrics
            .listener-metric
              span.label Active / limit
              strong {{ formatCount(listener.activeConnections) }} / {{ formatCount(listener.maxConnections) }}
            .listener-metric
              span.label Accepted
              strong {{ formatCount(listener.acceptedConnections) }}
            .listener-metric
              span.label Received
              strong {{ formatBytes(listener.bytesReceived) }}
            .listener-metric
              span.label Sent
              strong {{ formatBytes(listener.bytesSent) }}

  section.stream-section(aria-labelledby="stream-heading")
    .section-heading
      div
        p.eyebrow Runtime inventory
        h2#stream-heading Active RTMP streams
      p.snapshot-time(v-if="catalog") Snapshot {{ formatTime(catalog.as_of_unix_ms) }}

    .empty-state(v-if="catalog && catalog.streams.length === 0")
      span.empty-mark 00
      div
        h3 No active stream sessions
        p Publishers appear here while they are connected to a configured RTMP listener.

    .stream-grid(v-else-if="catalog")
      article.stream-card(v-for="stream in catalog.streams" :key="stream.id")
        header.stream-header
          div
            p.stream-server {{ stream.server_id }}
            h3 {{ stream.application }} / {{ stream.name }}
          span.live-pill(:class="{ dormant: !stream.publisher }")
            span.dot
            | {{ stream.publisher ? 'Publishing' : 'Waiting' }}

        .stream-facts
          .fact
            span.label Audience
            strong {{ stream.subscriber_count }} {{ stream.subscriber_count === 1 ? 'viewer' : 'viewers' }}
          .fact
            span.label Session age
            strong {{ formatAge(stream.created_at_unix_ms) }}
          .fact
            span.label Fanout queued
            strong {{ formatBytes(stream.media.fanout_payload_bytes) }}

        .track-grid
          .track
            .track-heading
              span.track-kind Audio
              strong {{ codecLabel(stream.media.audio) }}
            span.track-bytes {{ formatBytes(stream.media.audio.payload_bytes) }} received
            span.track-time RTMP {{ timestampLabel(stream.media.audio.last_rtmp_timestamp_ms) }}
          .track
            .track-heading
              span.track-kind Video
              strong {{ codecLabel(stream.media.video) }}
            span.track-bytes {{ formatBytes(stream.media.video.payload_bytes) }} received
            span.track-time RTMP {{ timestampLabel(stream.media.video.last_rtmp_timestamp_ms) }}

        section.recorder-panel(aria-label="Recording controls")
          .recorder-heading
            h4 Recorders
            span {{ stream.recorders.length }} configured
          p.no-recorders(v-if="stream.recorders.length === 0") No recorder is attached to this publisher.
          .recorder-row(v-for="recorder in stream.recorders" :key="recorder.id")
            .recorder-identity
              span.recorder-name {{ recorder.name ?? 'default recorder' }}
              span.recorder-phase(:class="`phase-${recorder.phase.state}`") {{ recorder.phase.state }}
              span.recorder-bytes {{ formatBytes(recorder.bytes_written) }} written
            button.record-button(
              v-if="recorder.manual"
              type="button"
              data-recorder-action
              :class="{ stop: recorder.phase.state === 'recording' }"
              :disabled="!canControlRecorder(recorder) || busyRecorder === recorder.id"
              @click="controlRecorder(stream, recorder)"
            ) {{ recorderActionLabel(recorder) }}
            span.automatic-badge(v-else) Automatic

  footer.page-footer
    span OxiRoute pre-alpha
    span.mono Live measurements / runtime inventory
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'

import {
  fetchMonitoring,
  fetchRtmpCatalog,
  setRecording,
  type MonitoringSnapshot,
  type RecorderSnapshot,
  type RtmpCatalog,
  type StreamSnapshot,
  type TrackSnapshot,
} from './api'

const REFRESH_INTERVAL_MS = 5_000
const STALE_AFTER_MS = REFRESH_INTERVAL_MS * 3
const numberFormatter = new Intl.NumberFormat()

const monitoring = ref<MonitoringSnapshot | null>(null)
const catalog = ref<RtmpCatalog | null>(null)
const monitoringError = ref<string | null>(null)
const catalogError = ref<string | null>(null)
const refreshing = ref(true)
const busyRecorder = ref<string | null>(null)
const currentUnixMs = ref(Date.now())
let refreshTimer: number | undefined
let activeController: AbortController | undefined
let activeRefresh: Promise<void> | null = null

const totalTrafficBytes = computed(
  () => (monitoring.value?.traffic.bytesReceived ?? 0) + (monitoring.value?.traffic.bytesSent ?? 0),
)
const usedMemoryBytes = computed(() => {
  if (!monitoring.value) return 0
  return Math.max(0, monitoring.value.host.totalMemoryBytes - monitoring.value.host.availableMemoryBytes)
})
const memoryUsagePercent = computed(() => {
  const total = monitoring.value?.host.totalMemoryBytes ?? 0
  return total === 0 ? 0 : Math.min(100, Math.max(0, (usedMemoryBytes.value / total) * 100))
})
const hostLoads = computed(() => [
  { label: '01m', value: monitoring.value?.host.loadAverage1m ?? 0 },
  { label: '05m', value: monitoring.value?.host.loadAverage5m ?? 0 },
  { label: '15m', value: monitoring.value?.host.loadAverage15m ?? 0 },
])
const maximumHostLoad = computed(() => Math.max(1, ...hostLoads.value.map(({ value }) => value)))
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
  return `The latest sample is ${formatSampleAge(monitoring.value.sampledAtUnixMs)}.`
})

function refresh(): Promise<void> {
  currentUnixMs.value = Date.now()
  if (activeRefresh) return activeRefresh

  const controller = new AbortController()
  activeController = controller
  refreshing.value = true

  activeRefresh = refreshData(controller).finally(() => {
    if (activeController === controller) activeController = undefined
    currentUnixMs.value = Date.now()
    refreshing.value = false
    activeRefresh = null
  })
  return activeRefresh
}

async function refreshData(controller: AbortController): Promise<void> {
  const [monitoringResult, catalogResult] = await Promise.allSettled([
    fetchMonitoring(controller.signal),
    fetchRtmpCatalog(controller.signal),
  ])
  if (controller.signal.aborted) return

  if (monitoringResult.status === 'fulfilled') {
    monitoring.value = monitoringResult.value
    monitoringError.value = null
  } else {
    monitoringError.value = errorMessage(monitoringResult.reason, 'Unable to load monitoring telemetry')
  }

  if (catalogResult.status === 'fulfilled') {
    catalog.value = catalogResult.value
    catalogError.value = null
  } else {
    catalogError.value = errorMessage(catalogResult.reason, 'Unable to load RTMP state')
  }
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback
}

async function controlRecorder(
  stream: StreamSnapshot,
  recorder: RecorderSnapshot,
): Promise<void> {
  const action = recorder.phase.state === 'recording' ? 'stop' : 'start'
  busyRecorder.value = recorder.id
  try {
    await setRecording(stream.id, recorder.id, action)
    await refresh()
  } catch (requestError) {
    catalogError.value = errorMessage(requestError, 'Recorder command failed')
  } finally {
    busyRecorder.value = null
  }
}

function canControlRecorder(recorder: RecorderSnapshot): boolean {
  if (!catalog.value?.capabilities.manual_recording) return false
  return !['starting', 'stopping'].includes(recorder.phase.state)
}

function recorderActionLabel(recorder: RecorderSnapshot): string {
  if (!catalog.value?.capabilities.manual_recording) return 'Backend unavailable'
  if (busyRecorder.value === recorder.id) return 'Sending...'
  switch (recorder.phase.state) {
    case 'recording':
      return 'Stop recording'
    case 'starting':
      return 'Starting...'
    case 'stopping':
      return 'Stopping...'
    default:
      return 'Start recording'
  }
}

function codecLabel(track: TrackSnapshot): string {
  if (track.codec_name) return track.codec_name.toUpperCase()
  return track.codec_id === null ? 'No signal' : `Codec ${track.codec_id}`
}

function timestampLabel(timestamp: number | null): string {
  return timestamp === null ? '--' : `${timestamp} ms`
}

function formatBytes(value: number | string): string {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const amount = bytes / 1024 ** exponent
  return `${amount >= 10 || exponent === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[exponent]}`
}

function formatCount(value: number): string {
  return numberFormatter.format(value)
}

function formatDecimal(value: number): string {
  return value.toFixed(2)
}

function formatPercent(value: number): string {
  return `${value.toFixed(1).replace(/\.0$/, '')}%`
}

function formatOptionalPercent(value: number | null): string {
  return value === null ? 'Unavailable' : formatPercent(value)
}

function formatDuration(durationMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(durationMs / 1000))
  const days = Math.floor(totalSeconds / 86_400)
  const hours = Math.floor((totalSeconds % 86_400) / 3_600)
  const minutes = Math.floor((totalSeconds % 3_600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m ${totalSeconds % 60}s`
  return `${totalSeconds}s`
}

function formatSampleAge(sampledAt: number): string {
  const seconds = Math.max(0, Math.floor((currentUnixMs.value - sampledAt) / 1000))
  if (seconds < 2) return 'just now'
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.floor(seconds / 60)
  return minutes < 60 ? `${minutes}m ago` : `${Math.floor(minutes / 60)}h ago`
}

function loadBarWidth(value: number): string {
  if (value <= 0) return '0%'
  return `${Math.max(4, (value / maximumHostLoad.value) * 100)}%`
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(timestamp)
}

function formatAge(startedAt: number): string {
  const seconds = Math.max(0, Math.floor((currentUnixMs.value - startedAt) / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

onMounted(() => {
  void refresh()
  refreshTimer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS)
})

onUnmounted(() => {
  activeController?.abort()
  if (refreshTimer !== undefined) window.clearInterval(refreshTimer)
})
</script>

<style scoped>
:global(*) {
  box-sizing: border-box;
}

:global(body) {
  margin: 0;
  min-width: 320px;
  color: #e9eddf;
  background:
    radial-gradient(circle at 80% -10%, rgb(182 255 81 / 10%), transparent 34rem),
    linear-gradient(180deg, #11130f 0%, #0b0d0a 100%);
  font-family: Inter, ui-sans-serif, system-ui, sans-serif;
}

:global(button) {
  font: inherit;
}

.console-shell {
  width: min(1480px, 100%);
  min-height: 100vh;
  margin: 0 auto;
  padding: 28px clamp(18px, 4vw, 64px) 32px;
}

.masthead,
.readout-bar,
.section-heading,
.stream-header,
.track-heading,
.recorder-row,
.page-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.masthead {
  padding-bottom: 26px;
  border-bottom: 1px solid #34392f;
}

.eyebrow,
.label,
.stream-server,
.track-kind {
  margin: 0;
  color: #929a88;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.7rem;
  font-weight: 650;
  letter-spacing: 0.14em;
  text-transform: uppercase;
}

h1,
h2,
h3,
h4,
p {
  margin-top: 0;
}

h1 {
  margin-bottom: 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(3.1rem, 8vw, 6.6rem);
  font-weight: 400;
  letter-spacing: -0.075em;
  line-height: 0.84;
}

.deck {
  margin: 10px 0 0 4px;
  color: #b6ff51;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  letter-spacing: 0.06em;
}

.system-state {
  display: flex;
  align-items: center;
  gap: 9px;
  color: #cbd0c2;
  font-size: 0.8rem;
}

.state-light,
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #b6ff51;
  box-shadow: 0 0 14px rgb(182 255 81 / 68%);
}

.system-state.alert .state-light {
  background: #ff745c;
  box-shadow: 0 0 14px rgb(255 116 92 / 68%);
}

.system-state.stale .state-light {
  background: #ffbf4b;
  box-shadow: 0 0 14px rgb(255 191 75 / 58%);
}

.readout-bar {
  align-items: stretch;
  border-bottom: 1px solid #34392f;
}

.readout {
  display: grid;
  flex: 1;
  gap: 8px;
  padding: 18px 0;
  border-right: 1px solid #34392f;
}

.readout strong {
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.7rem;
  font-weight: 400;
}

.mono {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace !important;
}

.refresh-button,
.record-button {
  border: 1px solid #b6ff51;
  color: #10140b;
  background: #b6ff51;
  cursor: pointer;
  font-weight: 750;
  transition: transform 120ms ease, background 120ms ease;
}

.refresh-button {
  align-self: center;
  margin-left: auto;
  padding: 10px 15px;
}

.refresh-button:hover:not(:disabled),
.record-button:hover:not(:disabled) {
  transform: translateY(-1px);
  background: #d2ff92;
}

button:focus-visible {
  outline: 3px solid #ffffff;
  outline-offset: 3px;
}

button:disabled {
  border-color: #555b4e;
  color: #8a9081;
  background: #252922;
  cursor: not-allowed;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
}

.capability-notice {
  border-color: #ffbf4b;
}

.error-notice {
  border-color: #ff745c;
}

.stale-notice {
  border-color: #ffbf4b;
}

.loading-state {
  display: flex;
  align-items: center;
  gap: 18px;
  min-height: 150px;
  margin-top: 24px;
  padding: 24px;
  border: 1px solid #34392f;
  background: rgb(23 26 21 / 72%);
}

.loading-state strong {
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.35rem;
  font-weight: 400;
}

.loading-state p {
  margin: 6px 0 0;
  color: #8f9788;
}

.loading-mark {
  width: 18px;
  height: 18px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: telemetry-spin 800ms linear infinite;
}

.monitoring-overview {
  padding-top: clamp(42px, 7vw, 74px);
}

.monitoring-heading {
  padding-bottom: 4px;
}

.monitor-grid {
  display: grid;
  grid-template-columns: repeat(12, minmax(0, 1fr));
  gap: 14px;
}

.monitor-panel {
  position: relative;
  min-height: 300px;
  padding: clamp(18px, 2.2vw, 28px);
  overflow: hidden;
  border: 1px solid #3a4034;
  background:
    linear-gradient(135deg, rgb(182 255 81 / 4%), transparent 45%),
    rgb(23 26 21 / 90%);
  box-shadow: 0 20px 80px rgb(0 0 0 / 18%);
}

.traffic-panel {
  grid-column: span 7;
}

.host-panel {
  grid-column: span 5;
}

.process-panel,
.rtmp-panel {
  grid-column: span 6;
}

.panel-heading,
.traffic-primary,
.memory-copy,
.listener-heading,
.listener-identity,
.rtmp-signal {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.monitor-panel h3,
.listener-heading h3 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(1.55rem, 3vw, 2.25rem);
  font-weight: 400;
  letter-spacing: -0.035em;
}

.panel-index {
  color: #4c5347;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 2.5rem;
}

.traffic-primary {
  align-items: end;
  margin: clamp(36px, 5vw, 62px) 0 25px;
}

.traffic-primary > div:first-child,
.process-primary,
.rtmp-signal > div {
  display: grid;
  gap: 5px;
}

.metric-display {
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(2.8rem, 7vw, 5.6rem);
  font-weight: 400;
  letter-spacing: -0.065em;
  line-height: 0.88;
}

.metric-caption,
.available-copy {
  color: #8f9788;
  font-size: 0.76rem;
}

.connection-total {
  display: grid;
  gap: 6px;
  padding-left: 18px;
  border-left: 1px solid #434a3d;
}

.connection-total strong {
  font-size: 1.15rem;
}

.direction-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}

.direction-card {
  display: flex;
  align-items: center;
  gap: 13px;
  padding: 14px;
  background: #10130e;
}

.direction-card > div {
  display: grid;
  gap: 5px;
}

.direction-arrow {
  display: grid;
  min-width: 34px;
  height: 34px;
  place-items: center;
  border: 1px solid #61734f;
  color: #b6ff51;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.58rem;
  font-weight: 750;
}

.direction-arrow.outbound {
  border-color: #5c5473;
  color: #b8a6ff;
}

.load-list {
  display: grid;
  gap: 16px;
  margin: 36px 0 28px;
}

.load-row {
  display: grid;
  grid-template-columns: 34px 1fr 42px;
  align-items: center;
  gap: 12px;
}

.load-window,
.listener-identity code {
  color: #8d9584;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.7rem;
}

.load-row strong {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.75rem;
  font-weight: 500;
  text-align: right;
}

.load-track,
.memory-track {
  height: 5px;
  overflow: hidden;
  background: #30352d;
}

.load-track span,
.memory-track span {
  display: block;
  height: 100%;
  background: #b6ff51;
}

.memory-readout {
  padding-top: 19px;
  border-top: 1px solid #34392f;
}

.memory-copy {
  align-items: baseline;
}

.memory-copy strong {
  font-size: 0.86rem;
}

.memory-track {
  height: 9px;
  margin: 12px 0 8px;
}

.process-primary {
  margin: 42px 0 30px;
}

.process-primary .metric-display {
  color: #b6ff51;
}

.process-grid,
.rtmp-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  border-block: 1px solid #34392f;
}

.compact-metric {
  display: grid;
  gap: 6px;
  padding: 15px 14px 15px 0;
}

.compact-metric:nth-child(even) {
  padding-left: 14px;
  border-left: 1px solid #34392f;
}

.compact-metric:nth-child(n + 3) {
  border-top: 1px solid #34392f;
}

.compact-metric strong {
  font-size: 0.92rem;
}

.rtmp-panel {
  background:
    radial-gradient(circle at 80% 28%, rgb(182 255 81 / 9%), transparent 12rem),
    rgb(23 26 21 / 90%);
}

.rtmp-signal {
  justify-content: flex-start;
  margin: 38px 0 27px;
}

.signal-ring {
  position: relative;
  width: 82px;
  height: 82px;
  border: 1px solid #536544;
  border-radius: 50%;
}

.signal-ring::before,
.signal-ring::after {
  position: absolute;
  border-radius: 50%;
  content: "";
}

.signal-ring::before {
  inset: 13px;
  border: 1px solid #78995b;
}

.signal-ring::after {
  inset: 31px;
  background: #b6ff51;
  box-shadow: 0 0 20px rgb(182 255 81 / 70%);
}

.rtmp-signal .metric-display {
  font-size: clamp(2.8rem, 6vw, 4.8rem);
}

.rtmp-grid {
  grid-template-columns: 1fr 1fr 1.35fr;
}

.rtmp-grid .compact-metric:nth-child(n) {
  padding-left: 14px;
  border-top: 0;
  border-left: 1px solid #34392f;
}

.rtmp-grid .compact-metric:first-child {
  padding-left: 0;
  border-left: 0;
}

.listener-section {
  margin-top: 14px;
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 76%);
}

.listener-heading {
  padding: 20px clamp(18px, 2.2vw, 28px);
  border-bottom: 1px solid #34392f;
}

.listener-count {
  color: #8f9788;
  font-size: 0.75rem;
}

.listener-empty {
  margin: 0;
  padding: 28px;
  color: #8f9788;
}

.listener-row {
  display: grid;
  grid-template-columns: minmax(220px, 0.8fr) 2fr;
  align-items: center;
  gap: 24px;
  padding: 18px clamp(18px, 2.2vw, 28px);
}

.listener-row + .listener-row {
  border-top: 1px solid #34392f;
}

.listener-identity {
  justify-content: flex-start;
}

.listener-identity h4 {
  margin: 0 0 5px;
  font-size: 0.92rem;
}

.listener-identity code {
  overflow-wrap: anywhere;
}

.protocol-badge {
  width: 48px;
  padding: 7px 5px;
  border: 1px solid #5f6a56;
  color: #c5d1b8;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.63rem;
  font-weight: 700;
  text-align: center;
  text-transform: uppercase;
}

.protocol-http {
  border-color: #5b7269;
  color: #a7ded0;
}

.protocol-rtmp {
  border-color: #6a6246;
  color: #efcf75;
}

.listener-metrics {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
}

.listener-metric {
  display: grid;
  gap: 6px;
  padding: 4px 14px;
}

.listener-metric + .listener-metric {
  border-left: 1px solid #34392f;
}

.listener-metric strong {
  font-size: 0.85rem;
}

.stream-section {
  padding: clamp(38px, 7vw, 74px) 0;
}

.section-heading {
  align-items: end;
  margin-bottom: 22px;
}

h2 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(2rem, 5vw, 3.7rem);
  font-weight: 400;
  letter-spacing: -0.045em;
}

.snapshot-time {
  margin-bottom: 5px;
  color: #7f8777;
  font-size: 0.8rem;
}

.stream-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
  gap: 18px;
}

.stream-card,
.empty-state {
  border: 1px solid #3a4034;
  background: rgb(23 26 21 / 88%);
  box-shadow: 0 20px 80px rgb(0 0 0 / 18%);
}

.stream-card {
  padding: 20px;
}

.stream-header {
  align-items: start;
}

.stream-header h3 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.8rem;
  font-weight: 400;
}

.live-pill {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 7px 9px;
  border: 1px solid #536544;
  color: #cde8a5;
  font-size: 0.72rem;
  text-transform: uppercase;
}

.live-pill.dormant {
  border-color: #555b4e;
  color: #959b8e;
}

.live-pill.dormant .dot {
  background: #73796d;
  box-shadow: none;
}

.stream-facts {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  margin: 22px 0;
  border-block: 1px solid #34392f;
}

.fact {
  display: grid;
  gap: 7px;
  padding: 15px 12px 15px 0;
}

.fact + .fact {
  padding-left: 12px;
  border-left: 1px solid #34392f;
}

.fact strong {
  font-size: 0.9rem;
  font-weight: 600;
}

.track-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}

.track {
  display: grid;
  gap: 7px;
  padding: 13px;
  background: #10130e;
}

.track-heading strong {
  color: #b6ff51;
  font-size: 0.9rem;
}

.track-bytes,
.track-time,
.recorder-bytes {
  color: #878f7e;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.7rem;
}

.recorder-panel {
  margin-top: 18px;
}

.recorder-heading {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 8px;
}

.recorder-heading h4 {
  margin: 0;
  font-size: 0.8rem;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.recorder-heading span,
.no-recorders {
  color: #7f8777;
  font-size: 0.75rem;
}

.recorder-row {
  padding: 12px 0;
  border-top: 1px solid #34392f;
}

.recorder-identity {
  display: grid;
  grid-template-columns: auto auto;
  gap: 4px 9px;
}

.recorder-name {
  font-weight: 650;
}

.recorder-phase,
.automatic-badge {
  width: fit-content;
  color: #b7bdaf;
  font-size: 0.68rem;
  font-weight: 750;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.phase-recording {
  color: #ff745c;
}

.phase-starting,
.phase-stopping {
  color: #ffbf4b;
}

.phase-failed {
  color: #ff745c;
}

.recorder-bytes {
  grid-column: 1 / -1;
}

.record-button {
  min-width: 125px;
  padding: 9px 12px;
  font-size: 0.75rem;
}

.record-button.stop {
  border-color: #ff745c;
  color: #ffe6e1;
  background: #4b211b;
}

.empty-state {
  display: flex;
  align-items: center;
  gap: 22px;
  padding: clamp(24px, 5vw, 52px);
}

.empty-mark {
  color: #b6ff51;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 4rem;
}

.empty-state h3 {
  margin-bottom: 6px;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.5rem;
  font-weight: 400;
}

.empty-state p {
  margin-bottom: 0;
  color: #8f9788;
}

.page-footer {
  padding-top: 18px;
  border-top: 1px solid #34392f;
  color: #697062;
  font-size: 0.7rem;
}

@keyframes telemetry-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 1050px) {
  .traffic-panel,
  .host-panel,
  .process-panel,
  .rtmp-panel {
    grid-column: span 6;
  }

  .listener-row {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 760px) {
  .traffic-panel,
  .host-panel,
  .process-panel,
  .rtmp-panel {
    grid-column: 1 / -1;
  }
}

@media (max-width: 680px) {
  .console-shell {
    padding-inline: 16px;
  }

  .masthead,
  .section-heading,
  .recorder-row,
  .page-footer {
    align-items: flex-start;
    flex-direction: column;
  }

  .system-state {
    align-self: flex-start;
  }

  .readout-bar {
    display: grid;
    grid-template-columns: 1fr 1fr;
  }

  .readout:nth-child(even) {
    border-right: 0;
  }

  .refresh-button {
    width: 100%;
    margin: 0 0 16px;
    grid-column: 1 / -1;
  }

  .stream-facts,
  .track-grid {
    grid-template-columns: 1fr;
  }

  .traffic-primary {
    align-items: flex-start;
    flex-direction: column;
  }

  .connection-total {
    padding: 12px 0 0;
    border-top: 1px solid #434a3d;
    border-left: 0;
  }

  .listener-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .listener-metrics {
    grid-template-columns: 1fr 1fr;
  }

  .listener-metric:nth-child(3) {
    border-left: 0;
  }

  .listener-metric:nth-child(n + 3) {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid #34392f;
  }

  .fact + .fact {
    padding-left: 0;
    border-top: 1px solid #34392f;
    border-left: 0;
  }

  .record-button {
    width: 100%;
  }
}

@media (max-width: 420px) {
  .readout-bar,
  .direction-grid,
  .rtmp-grid {
    grid-template-columns: 1fr;
  }

  .readout {
    border-right: 0;
  }

  .readout + .readout {
    border-top: 1px solid #34392f;
  }

  .rtmp-grid .compact-metric:nth-child(n) {
    padding-left: 0;
    border-top: 1px solid #34392f;
    border-left: 0;
  }

  .rtmp-grid .compact-metric:first-child {
    border-top: 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
