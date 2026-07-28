<template lang="pug">
section.recorder-panel(aria-label="Recording controls")
  .recorder-heading
    div
      h4 Recorders
      span.recording-support(:class="{ unsupported: !recordingSupported }") {{ recordingSupportLabel }}
    span {{ stream.recorders.length }} configured
  p.no-recorders(v-if="stream.recorders.length === 0") No recorder is attached to this publisher.
  article.recorder-row(v-for="recorder in stream.recorders" :key="recorder.id")
    .recorder-identity
      .recorder-title
        span.recorder-name {{ recorder.name ?? 'default recorder' }}
        span.recorder-mode {{ recorder.manual ? 'Manual' : 'Continuous' }}
        span.recorder-phase(:class="`phase-${recorder.phase.state}`") {{ recorder.phase.state }}
      .recorder-metrics
        span {{ formatBytes(recorder.bytes_written) }} written
        span {{ pluralizedCount(recorder.segments_completed, 'segment', 'segments') }}
        span {{ pluralizedCount(recorder.discontinuities, 'discontinuity', 'discontinuities') }}
      dl.recorder-files(v-if="recorder.last_completed_relative_name || recorder.recoverable_partial_name")
        div(v-if="recorder.last_completed_relative_name")
          dt Completed
          dd
            code {{ recorder.last_completed_relative_name }}
        div(v-if="recorder.recoverable_partial_name")
          dt Recoverable
          dd
            code {{ recorder.recoverable_partial_name }}
    button.record-button(
      v-if="recorder.manual"
      type="button"
      data-recorder-action
      :data-recorder-id="recorder.id"
      :class="{ stop: recorder.phase.state === 'recording' }"
      :disabled="controlAction(recorder) === null || busyRecorderId === recorder.id"
      :title="recorderControlReason(recorder)"
      @click="emitControl(recorder)"
    ) {{ recorderActionLabel(recorder) }}
    span.automatic-badge(v-else) Automatic
  .recorder-heading.relay-heading
    div
      h4 Push relays
    span {{ stream.relays.length }} configured
  p.no-recorders(v-if="stream.relays.length === 0") No push relay is attached to this publisher.
  article.recorder-row(v-for="relay in stream.relays" :key="relay.id")
    .recorder-identity
      .recorder-title
        span.recorder-name {{ relay.destination.address }} / {{ relay.destination.application }}
        span.recorder-phase(:class="`phase-${relay.phase}`") {{ relay.phase }}
      .recorder-metrics
        span {{ formatBytes(relay.payload_bytes_sent) }} sent
        span {{ pluralizedCount(relay.reconnects, 'reconnect', 'reconnects') }}
        span {{ pluralizedCount(relay.events_dropped, 'drop', 'drops') }}
      small(v-if="relay.last_failure") Last operational failure: {{ relay.last_failure }}
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type { RecorderSnapshot, StreamSnapshot } from './api'
import { formatBytes, formatCount } from './formatters'
import {
  hasObservedCodec,
  recorderControlAction,
  streamRecordingSupported,
} from './recording'

const props = defineProps<{
  stream: StreamSnapshot
  manualRecording: boolean
  busyRecorderId: string | null
}>()

const emit = defineEmits<{
  control: [recorder: RecorderSnapshot]
}>()

const recordingSupported = computed(() => streamRecordingSupported(props.stream))

const recordingSupportLabel = computed(() => {
  if (!props.stream.recording_supported) return 'No recorder configured'
  if (recordingSupported.value) return 'Recording supported'
  return [props.stream.media.audio, props.stream.media.video].some(hasObservedCodec)
    ? 'Active codec cannot be recorded'
    : 'Waiting for recordable media'
})

function controlAction(recorder: RecorderSnapshot): 'start' | 'stop' | null {
  return recorderControlAction(props.manualRecording, props.stream, recorder)
}

function emitControl(recorder: RecorderSnapshot): void {
  if (controlAction(recorder)) emit('control', recorder)
}

function recorderActionLabel(recorder: RecorderSnapshot): string {
  if (!props.manualRecording || !props.stream.manual_recording) return 'Manual control unavailable'
  if (props.busyRecorderId === recorder.id) return 'Sending...'
  switch (recorder.phase.state) {
    case 'recording': return 'Stop recording'
    case 'starting': return 'Starting...'
    case 'stopping': return 'Stopping...'
    case 'failed': return recordingSupported.value ? 'Retry recording' : 'Recording unsupported'
    default: return recordingSupported.value ? 'Start recording' : 'Recording unsupported'
  }
}

function recorderControlReason(recorder: RecorderSnapshot): string | undefined {
  if (props.busyRecorderId === recorder.id) return 'A recorder command is in progress.'
  if (!props.manualRecording) return 'The active runtime does not expose manual recording commands.'
  if (!props.stream.manual_recording) return 'This stream has no manual recorder.'
  if (!props.stream.publisher) return 'A publisher must be attached before recording can be controlled.'
  if (['starting', 'stopping'].includes(recorder.phase.state)) return 'The recorder is already changing state.'
  if (recorder.phase.state !== 'recording' && !recordingSupported.value) {
    return 'Observed media codecs are not currently recordable.'
  }
  return undefined
}

function pluralizedCount(value: string, singular: string, plural: string): string {
  return `${formatCount(value)} ${BigInt(value) === 1n ? singular : plural}`
}
</script>

<style scoped>
.recorder-heading,
.recorder-row,
.recorder-title,
.recorder-metrics {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.recorder-panel {
  margin-top: 20px;
  padding-top: 18px;
  border-top: 1px solid #30362d;
}

.recorder-heading {
  margin-bottom: 12px;
}

.recorder-heading h4 {
  margin: 0;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 1.05rem;
  font-weight: 400;
}

.recorder-heading > span,
.recording-support,
.no-recorders {
  color: #858d7d;
  font-size: 0.7rem;
}

.recording-support {
  display: block;
  margin-top: 4px;
  color: #b6d98c;
}

.recording-support.unsupported {
  color: #d2ad68;
}

.recorder-row {
  align-items: flex-start;
  padding: 12px;
  border: 1px solid #30362d;
  background: #0e110d;
}

.recorder-row + .recorder-row {
  margin-top: 8px;
}

.recorder-identity {
  display: grid;
  min-width: 0;
  gap: 8px;
}

.recorder-title,
.recorder-metrics {
  justify-content: flex-start;
  flex-wrap: wrap;
}

.recorder-name {
  color: #dce3d4;
  font-weight: 700;
}

.recorder-mode,
.recorder-phase,
.recorder-metrics,
.automatic-badge {
  color: #8d9585;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.64rem;
  letter-spacing: 0.07em;
  text-transform: uppercase;
}

.recorder-phase,
.recorder-mode,
.automatic-badge {
  padding: 3px 6px;
  border: 1px solid #42493c;
}

.phase-recording {
  border-color: #6b8a4d;
  color: #b6ff51;
}

.phase-failed {
  border-color: #75483f;
  color: #ff9b88;
}

.recorder-files {
  display: grid;
  gap: 5px;
  margin: 0;
}

.recorder-files div {
  display: grid;
  grid-template-columns: 80px minmax(0, 1fr);
  gap: 8px;
}

.recorder-files dt,
.recorder-files dd {
  min-width: 0;
  margin: 0;
  color: #858d7d;
  font-size: 0.68rem;
}

.recorder-files code {
  overflow-wrap: anywhere;
  color: #bdc8b4;
}

.record-button {
  min-height: 38px;
  padding: 8px 11px;
  border: 1px solid #748d5e;
  color: #c7ef94;
  background: transparent;
  cursor: pointer;
  white-space: nowrap;
}

.record-button.stop {
  border-color: #7d554a;
  color: #ffad9d;
}

.record-button:disabled {
  border-color: #3c4238;
  color: #70786a;
  cursor: not-allowed;
}

.record-button:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

@media (max-width: 700px) {
  .recorder-row {
    align-items: stretch;
    flex-direction: column;
  }

  .record-button {
    width: 100%;
    min-height: 44px;
  }
}
</style>
