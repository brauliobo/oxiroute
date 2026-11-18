<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import type { RecorderSnapshot, StreamSnapshot } from './api'
import { formatBytes, pluralize } from './formatters'
import { hasObservedCodec, recorderControlAction, recorderPhaseTone, relayPhaseTone, streamRecordingSupported } from './recording'
import './overview/rtmp.css'

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
</script>

<template lang="pug">
section.ox-recorder(data-section="recorders" aria-label="Recording controls")
  .ox-card-head
    div
      h4 Recorders
      p.ox-muted {{ recordingSupportLabel }}
    span.ox-muted {{ stream.recorders.length }} configured
  p.ox-muted(v-if="stream.recorders.length === 0") No recorder is attached to this publisher.
  article.ox-recorder-row(v-for="recorder in stream.recorders" :key="recorder.id")
    .ox-recorder-body
      .ox-recorder-title
        strong {{ recorder.name ?? 'default recorder' }}
        span.ox-muted {{ recorder.manual ? 'Manual' : 'Continuous' }}
        Badge(:tone="recorderPhaseTone(recorder.phase.state)" :label="recorder.phase.state.toUpperCase()")
      p.ox-muted {{ formatBytes(recorder.bytes_written) }} written / {{ pluralize(recorder.segments_completed, 'segment') }} / {{ pluralize(recorder.discontinuities, 'discontinuity', 'discontinuities') }}
      dl.ox-recorder-files(v-if="recorder.last_completed_relative_name || recorder.recoverable_partial_name")
        div(v-if="recorder.last_completed_relative_name")
          dt Completed
          dd
            code {{ recorder.last_completed_relative_name }}
        div(v-if="recorder.recoverable_partial_name")
          dt Recoverable
          dd
            code {{ recorder.recoverable_partial_name }}
    Button(
      v-if="recorder.manual"
      data-recorder-action
      :data-recorder-id="recorder.id"
      :variant="recorder.phase.state === 'recording' ? 'danger' : 'default'"
      :disabled="controlAction(recorder) === null || busyRecorderId === recorder.id"
      :title="recorderControlReason(recorder)"
      @click="emitControl(recorder)"
    ) {{ recorderActionLabel(recorder) }}
    Badge(v-else tone="info" label="AUTOMATIC")
  .ox-card-head
    h4 Push relays
    span.ox-muted {{ stream.relays.length }} configured
  p.ox-muted(v-if="stream.relays.length === 0") No push relay is attached to this publisher.
  article.ox-recorder-row(v-for="relay in stream.relays" :key="relay.id")
    .ox-recorder-body
      .ox-recorder-title
        strong {{ relay.destination.address }} / {{ relay.destination.application }}
        Badge(:tone="relayPhaseTone(relay.phase)" :label="relay.phase.toUpperCase()")
      p.ox-muted {{ formatBytes(relay.payload_bytes_sent) }} sent / {{ pluralize(relay.reconnects, 'reconnect') }} / {{ pluralize(relay.events_dropped, 'drop') }}
      p.ox-bad-text(v-if="relay.last_failure") Last operational failure: {{ relay.last_failure }}
</template>
