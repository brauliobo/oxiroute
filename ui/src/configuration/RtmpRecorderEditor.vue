<template lang="pug">
ItemScope.route-card.recorder-card(:index="index")
  header.route-card-heading
    strong Recorder {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove recorder ${index + 1}`" @click="emit('remove')") Remove
  .field-grid
    ConfigField(path="rtmp_services[].applications[].recorders[].name" label="Stable name")
      input(type="text" v-model="recorder.name" required)
    ConfigField(path="rtmp_services[].applications[].recorders[].start" label="Start policy")
      select(v-model="recorder.start")
        option(value="continuous") Continuous
        option(value="manual") Manual control
    ConfigField(path="rtmp_services[].applications[].recorders[].root_directory" label="Recording root directory")
      input(type="text" v-model="recorder.root_directory" required placeholder="/var/lib/oxiroute/recordings")
  ConfigGroup.object-block(path="rtmp_services[].applications[].recorders[].record_mask" legend="Media selection")
    .field-grid
      ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].record_mask.audio" label="Include audio" check)
        input(type="checkbox" v-model="recorder.record_mask.audio")
      ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].record_mask.video" label="Include video" check)
        input(type="checkbox" v-model="recorder.record_mask.video")
      ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].record_mask.keyframes" label="Keep keyframes only" check)
        input(type="checkbox" v-model="recorder.record_mask.keyframes")
    ConfigField(path="rtmp_services[].applications[].recorders[].suffix_template" label="File suffix template")
      input(type="text" v-model="recorder.suffix_template" required placeholder="-%Y-%m-%dT%H-%M-%S.flv")
    ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].append_unix_seconds" label="Append Unix seconds to file names" check)
      input(type="checkbox" v-model="recorder.append_unix_seconds")
    ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].append" label="Resume a valid existing segment" check)
      input(type="checkbox" v-model="recorder.append")
    ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].lock" label="Lock the active file" check)
      input(type="checkbox" v-model="recorder.lock")
    ConfigField.compact-enable(path="rtmp_services[].applications[].recorders[].notify" label="Retain recorder notifications" check)
      input(type="checkbox" v-model="recorder.notify")
  .field-grid
    NullableLimitField(
      v-model="recorder.max_size"
      :default-value="10_737_418_240"
      field-path="rtmp_services[].applications[].recorders[].max_size"
      legend="Recording size"
      input-label="Maximum recording bytes"
      :max="1_099_511_627_776"
      unbounded-label="No recording size limit"
    )
    NullableLimitField(
      v-model="recorder.max_frames"
      :default-value="10_000_000"
      field-path="rtmp_services[].applications[].recorders[].max_frames"
      legend="Recording frames"
      input-label="Maximum recording frames"
      :max="1_000_000_000"
      unbounded-label="No recording frame limit"
    )
    ConfigField(path="rtmp_services[].applications[].recorders[].timezone" label="Filename timezone")
      input(type="text" v-model="recorder.timezone" required placeholder="America/Argentina/Buenos_Aires")
      small Use `utc` or an IANA timezone name.
    ConfigField(path="rtmp_services[].applications[].recorders[].time_basis" label="Filename time basis")
      select(v-model="recorder.time_basis")
        option(value="segment_start") Segment start
        option(value="segment_end") Segment end
    ConfigField(path="rtmp_services[].applications[].recorders[].segment_naming" label="Rotation naming")
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
      ConfigField(path="rtmp_services[].applications[].recorders[].max_queue_messages" label="Maximum queued messages")
        input(type="number" min="1" max="65536" step="1" v-model.number="recorder.max_queue_messages")
      ConfigField(path="rtmp_services[].applications[].recorders[].max_queue_bytes" label="Maximum queued bytes")
        input(type="number" min="1" :max="recorder.max_storage_bytes === null ? 1073741824 : Math.min(1073741824, recorder.max_storage_bytes)" step="1" v-model.number="recorder.max_queue_bytes")
        small Must not exceed a configured storage byte limit.
      ConfigField(path="rtmp_services[].applications[].recorders[].shutdown_timeout_ms" label="Shutdown timeout (ms)")
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
      ConfigField(path="rtmp_services[].applications[].recorders[].max_active_recorders" label="Maximum active recorders")
        input(type="number" min="1" max="256" step="1" v-model.number="recorder.max_active_recorders")
      ConfigField(path="rtmp_services[].applications[].recorders[].retention" label="When a storage quota is reached")
        Select(:model-value="recorder.retention" :options="retentionOptions" @update:modelValue="setRetention")
        small Delete oldest removes the oldest finished recordings in the root, never an active one.
    NullableLimitField(
      v-model="recorder.retention_max_age_ms"
      :default-value="2_592_000_000"
      field-path="rtmp_services[].applications[].recorders[].retention_max_age_ms"
      legend="Recording age"
      input-label="Maximum recording age (ms)"
      :max="315_360_000_000"
      unbounded-label="Recordings never expire"
    )
</template>

<script setup lang="ts" vapor>
import Select from '#design/field/Select.vue'
import type { SelectOption } from '#design/field/types'
import type { RtmpRecorderConfig, RtmpRecorderRetention } from '../config'
import NullableLimitField from './NullableLimitField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{
  recorder: RtmpRecorderConfig
  index: number
}>()

const emit = defineEmits<{ remove: [] }>()

const retentionOptions: SelectOption[] = [
  { value: 'reject', label: 'Reject new recordings' },
  { value: 'delete_oldest', label: 'Delete the oldest recordings' },
]

function setRetention(value: string): void {
  props.recorder.retention = value as RtmpRecorderRetention
}
</script>
