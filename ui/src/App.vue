<template lang="pug">
main.console-shell
  header.masthead
    .brand-block
      p.eyebrow Network control / RTMP
      h1 OxiRoute
      p.deck Broadcast desk
    .system-state(:class="{ alert: error }")
      span.state-light(aria-hidden="true")
      span {{ error ? 'API interrupted' : loading ? 'Synchronizing' : 'Control plane online' }}

  section.readout-bar(aria-label="RTMP summary")
    .readout
      span.label Active streams
      strong {{ catalog?.streams.length ?? 0 }}
    .readout
      span.label Connected viewers
      strong {{ totalViewers }}
    .readout
      span.label Catalog revision
      strong.mono {{ catalog?.revision ?? '--' }}
    button.refresh-button(type="button" @click="refresh" :disabled="loading") Refresh now

  p.notice.error-notice(v-if="error" role="alert") {{ error }}
  p.notice.capability-notice(v-if="catalog && !catalog.capabilities.live_ingest")
    strong RTMP ingestion is not connected.
    |  The visibility API is ready, but handshakes do not create synthetic streams.

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
        p Publishers and subscribers will appear here after RTMP command handling is connected.

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
    span.mono Disk/runtime separation remains visible by design
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'

import {
  fetchRtmpCatalog,
  setRecording,
  type RecorderSnapshot,
  type RtmpCatalog,
  type StreamSnapshot,
  type TrackSnapshot,
} from './api'

const catalog = ref<RtmpCatalog | null>(null)
const error = ref<string | null>(null)
const loading = ref(true)
const busyRecorder = ref<string | null>(null)
let refreshTimer: number | undefined
let activeRequest: AbortController | undefined

const totalViewers = computed(
  () => catalog.value?.streams.reduce((sum, stream) => sum + stream.subscriber_count, 0) ?? 0,
)

async function refresh(): Promise<void> {
  activeRequest?.abort()
  activeRequest = new AbortController()
  loading.value = true
  try {
    catalog.value = await fetchRtmpCatalog(activeRequest.signal)
    error.value = null
  } catch (requestError) {
    if (requestError instanceof DOMException && requestError.name === 'AbortError') return
    error.value = requestError instanceof Error ? requestError.message : 'Unable to load RTMP state'
  } finally {
    loading.value = false
  }
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
    error.value = requestError instanceof Error ? requestError.message : 'Recorder command failed'
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

function formatBytes(value: string): string {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const amount = bytes / 1024 ** exponent
  return `${amount >= 10 || exponent === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[exponent]}`
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(timestamp)
}

function formatAge(startedAt: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - startedAt) / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

onMounted(() => {
  void refresh()
  refreshTimer = window.setInterval(() => void refresh(), 5_000)
})

onUnmounted(() => {
  activeRequest?.abort()
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

  .fact + .fact {
    padding-left: 0;
    border-top: 1px solid #34392f;
    border-left: 0;
  }

  .record-button {
    width: 100%;
  }
}
</style>
