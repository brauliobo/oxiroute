<template lang="pug">
section.stream-section(v-show="visible" aria-labelledby="stream-heading")
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

      RtmpRecorderPanel(
        :stream="stream"
        :manual-recording="catalog.capabilities.manual_recording"
        :busy-recorder-id="busyRecorderId"
        @control="emit('control', stream, $event)"
      )
</template>

<script setup lang="ts" vapor>
import type { RecorderSnapshot, RtmpCatalog, StreamSnapshot, TrackSnapshot } from '../api'
import { formatBytes, formatClockTime as formatTime } from '../formatters'
import RtmpRecorderPanel from '../RtmpRecorderPanel.vue'

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

function timestampLabel(timestamp: number | null): string {
  return timestamp === null ? '--' : `${timestamp} ms`
}

function formatAge(startedAt: number): string {
  const seconds = Math.max(0, Math.floor((props.currentUnixMs - startedAt) / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}
</script>

<style scoped src="./base.css"></style>
<style scoped src="./StreamInventory.css"></style>
