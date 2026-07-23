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
        input(type="number" min="1" :max="Math.min(1073741824, recorder.max_storage_bytes)" step="1" v-model.number="recorder.max_queue_bytes")
        small Must not exceed the storage byte limit.
      label.field(data-field="rtmp_services[].applications[].recorders[].shutdown_timeout_ms")
        span Shutdown timeout (ms)
        input(type="number" min="1" max="60000" step="1" v-model.number="recorder.shutdown_timeout_ms")
  fieldset.object-block.recorder-limits
    legend Shared storage limits
    .field-grid
      label.field(data-field="rtmp_services[].applications[].recorders[].max_storage_bytes")
        span Maximum storage bytes
        input(type="number" min="1" max="1099511627776" step="1" v-model.number="recorder.max_storage_bytes")
      label.field(data-field="rtmp_services[].applications[].recorders[].max_storage_files")
        span Maximum storage files
        input(type="number" min="1" max="1000000" step="1" v-model.number="recorder.max_storage_files")
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
