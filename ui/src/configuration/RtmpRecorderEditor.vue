<template lang="pug">
article.route-card.recorder-card
  header.route-card-heading
    strong Recorder {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove recorder ${index + 1}`" @click="$emit('remove')") Remove
  .field-grid
    label.field(data-field="rtmp_services[].applications[].recorders[].name")
      span Stable name
      input(type="text" v-model="recorder.name" required)
    label.field(data-field="rtmp_services[].applications[].recorders[].start")
      span Start policy
      select(v-model="recorder.start")
        option(value="continuous") Continuous
        option(value="manual") Manual control
    label.field(data-field="rtmp_services[].applications[].recorders[].root_directory")
      span Recording root directory
      input(type="text" v-model="recorder.root_directory" required placeholder="/var/lib/oxiroute/recordings")
    label.field(data-field="rtmp_services[].applications[].recorders[].suffix_template")
      span File suffix template
      input(type="text" v-model="recorder.suffix_template" required placeholder="-%Y-%m-%dT%H-%M-%S.flv")
    label.enable-row.compact-enable(data-field="rtmp_services[].applications[].recorders[].append_unix_seconds")
      input(type="checkbox" v-model="recorder.append_unix_seconds")
      span Append Unix seconds to file names
    label.field(data-field="rtmp_services[].applications[].recorders[].timezone")
      span Filename timezone
      input(type="text" v-model="recorder.timezone" required placeholder="America/Bahia")
      small Use `utc` or an IANA timezone name.
    label.field(data-field="rtmp_services[].applications[].recorders[].time_basis")
      span Filename time basis
      select(v-model="recorder.time_basis")
        option(value="segment_start") Segment start
        option(value="segment_end") Segment end
    label.field(data-field="rtmp_services[].applications[].recorders[].segment_naming")
      span Rotation naming
      select(v-model="recorder.segment_naming")
        option(value="safe_unique") Sequenced safe names
        option(value="nginx_compatible") Rerender suffix each segment
  NullableLimitField(
    v-model="recorder.rotation_interval_ms"
    :default-value="60000"
    :max="2147483647"
    field-path="rtmp_services[].applications[].recorders[].rotation_interval_ms"
    legend="Segment rotation"
    input-label="Rotation interval (ms)"
    mode-label="Rotation mode"
    bounded-label="Interval"
    unbounded-label="No rotation"
  )
  fieldset.object-block.recorder-limits
    legend Queue and shutdown
    .field-grid
      label.field(data-field="rtmp_services[].applications[].recorders[].max_queue_messages")
        span Maximum queued messages
        input(type="number" min="1" max="65536" step="1" v-model.number="recorder.max_queue_messages")
      label.field(data-field="rtmp_services[].applications[].recorders[].max_queue_bytes")
        span Maximum queued bytes
        input(type="number" min="1" :max="recorder.max_storage_bytes === null ? 1073741824 : Math.min(1073741824, recorder.max_storage_bytes)" step="1" v-model.number="recorder.max_queue_bytes")
        small Must not exceed a configured storage byte limit.
      label.field(data-field="rtmp_services[].applications[].recorders[].shutdown_timeout_ms")
        span Shutdown timeout (ms)
        input(type="number" min="1" max="60000" step="1" v-model.number="recorder.shutdown_timeout_ms")
  fieldset.object-block.recorder-limits
    legend Shared storage limits
    NullableLimitField(
      v-model="recorder.max_storage_bytes"
      :default-value="10_737_418_240"
      field-path="rtmp_services[].applications[].recorders[].max_storage_bytes"
      legend="Storage bytes"
      input-label="Maximum storage bytes"
      :max="1_099_511_627_776"
      unbounded-label="No byte quota"
    )
    NullableLimitField(
      v-model="recorder.max_storage_files"
      :default-value="10_000"
      field-path="rtmp_services[].applications[].recorders[].max_storage_files"
      legend="Storage files"
      input-label="Maximum storage files"
      :max="1_000_000"
      unbounded-label="No file quota"
    )
    .field-grid
      label.field(data-field="rtmp_services[].applications[].recorders[].max_active_recorders")
        span Maximum active recorders
        input(type="number" min="1" max="256" step="1" v-model.number="recorder.max_active_recorders")
</template>

<script setup lang="ts">
import type { RtmpRecorderConfig } from '../config'
import NullableLimitField from './NullableLimitField.vue'

defineProps<{
  recorder: RtmpRecorderConfig
  index: number
}>()

defineEmits<{ remove: [] }>()
</script>
