<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Card from '#design/panel/Card.vue'
import Panel from '#design/panel/Panel.vue'
import type { RecorderSnapshot, RtmpCatalog, StreamSnapshot, TrackSnapshot } from '../api'
import { formatBytes, formatSnapshotTime, formatTelemetryDuration, pluralize } from '../formatters'
import RtmpRecorderPanel from '../RtmpRecorderPanel.vue'
import MetricList from './MetricList.vue'
import type { Metric } from './metric'
import './rtmp.css'

const props = defineProps<{
  catalog: RtmpCatalog | null
  currentUnixMs: number
  busyRecorderId: string | null
  visible: boolean
}>()

const emit = defineEmits<{
  control: [stream: StreamSnapshot, recorder: RecorderSnapshot]
}>()

function codecLabel(track: TrackSnapshot): string {
  if (track.codec_name) return track.codec_name.toUpperCase()
  if (track.codec_fourcc) return track.codec_fourcc.toUpperCase()
  return track.codec_id === null ? 'No signal' : `Codec ${track.codec_id}`
}

function trackLabel(track: TrackSnapshot): string {
  const timestamp = track.last_rtmp_timestamp_ms === null ? 'no timestamp' : `RTMP ${track.last_rtmp_timestamp_ms} ms`
  return `${codecLabel(track)} / ${formatBytes(track.payload_bytes)} received / ${timestamp}`
}

function facts(stream: StreamSnapshot): Metric[] {
  return [
    { label: 'Audience', value: pluralize(stream.subscriber_count, 'viewer') },
    { label: 'Session age', value: formatTelemetryDuration(Math.max(0, props.currentUnixMs - stream.created_at_unix_ms)) },
    { label: 'Fanout queued', value: formatBytes(stream.media.fanout_payload_bytes) },
    { label: 'Audio', value: trackLabel(stream.media.audio) },
    { label: 'Video', value: trackLabel(stream.media.video) },
  ]
}
</script>

<template lang="pug">
Panel.ox-view-section(v-show="visible" title="Active RTMP streams")
  p.ox-muted(v-if="catalog") Snapshot: {{ formatSnapshotTime(catalog.as_of_unix_ms) }}
  EmptyState(v-if="catalog && catalog.streams.length === 0" title="No active stream sessions") Publishers appear here while they are connected to a configured RTMP listener.
  .ox-cards(v-else-if="catalog")
    Card(v-for="stream in catalog.streams" :key="stream.id")
      .ox-card-head
        div
          h3 {{ stream.application }} / {{ stream.name }}
          p.ox-muted {{ stream.server_id }}
        Badge(:tone="stream.publisher ? 'ok' : 'neutral'" :label="stream.publisher ? 'PUBLISHING' : 'WAITING'")
      MetricList(:items="facts(stream)")
      RtmpRecorderPanel(
        :stream="stream"
        :manual-recording="catalog.capabilities.manual_recording"
        :busy-recorder-id="busyRecorderId"
        @control="emit('control', stream, $event)"
      )
</template>
