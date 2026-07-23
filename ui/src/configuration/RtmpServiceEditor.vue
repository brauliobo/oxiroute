<template lang="pug">
header.form-heading
  div
    p.eyebrow RTMP dispatch
    h3 {{ service.name || 'Unnamed RTMP service' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove service
.field-grid
  label.field(data-field="rtmp_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
fieldset.route-list(data-field="rtmp_services[].applications")
  .route-heading
    legend Applications
    button.add-row(
      type="button"
      :disabled="service.applications.length >= 256"
      :title="service.applications.length >= 256 ? 'The server allows at most 256 applications per RTMP service.' : undefined"
      @click="addApplication"
    ) + Add application
  p.empty-list(v-if="service.applications.length === 0") At least one application is required.
  article.route-card(v-for="(application, applicationIndex) in service.applications" :key="applicationIndex")
    header.route-card-heading
      strong Application {{ applicationIndex + 1 }}
      button.danger-link(type="button" :aria-label="`Remove RTMP application ${applicationIndex + 1}`" @click="removeApplication(applicationIndex)") Remove
    .field-grid
      label.field(data-field="rtmp_services[].applications[].name")
        span Application name
        input(type="text" v-model="application.name")
      label.enable-row.compact-enable(data-field="rtmp_services[].applications[].live")
        input(
          type="checkbox"
          v-model="application.live"
          :disabled="application.recorders.length > 0"
          :title="application.recorders.length > 0 ? 'Remove configured recorders before disabling live publishing.' : undefined"
        )
        span Allow live publishing
      label.enable-row.compact-enable(data-field="rtmp_services[].applications[].idle_streams")
        input(type="checkbox" v-model="application.idle_streams")
        span Allow viewers before a publisher
    fieldset.route-list.recorder-list(data-field="rtmp_services[].applications[].recorders")
      .route-heading
        legend Recorders
        button.add-row(
          type="button"
          :disabled="!application.live || application.recorders.length >= 8"
          :title="recorderAddReason(application)"
          @click="addRecorder(applicationIndex)"
        ) + Add recorder
      p.empty-list(v-if="application.recorders.length === 0") No recorder is configured for this application.
      RtmpRecorderEditor(
        v-for="(recorder, recorderIndex) in application.recorders"
        :key="recorderIndex"
        :recorder="recorder"
        :index="recorderIndex"
        @remove="removeRecorder(applicationIndex, recorderIndex)"
      )
</template>

<script setup lang="ts">
import type { RtmpApplicationConfig, RtmpRecorderConfig, RtmpServiceConfig } from '../config'
import RtmpRecorderEditor from './RtmpRecorderEditor.vue'

const props = defineProps<{ service: RtmpServiceConfig }>()
const emit = defineEmits<{
  changed: []
  remove: []
}>()

function newApplication(): RtmpApplicationConfig {
  return { name: '', live: true, idle_streams: true, recorders: [] }
}

function newRecorder(): RtmpRecorderConfig {
  return {
    name: '',
    start: 'continuous',
    root_directory: '/var/lib/oxiroute/recordings',
    suffix_template: '.flv',
    append_unix_seconds: false,
    rotation_interval_ms: null,
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    shutdown_timeout_ms: 5_000,
    max_storage_bytes: 10_737_418_240,
    max_storage_files: 10_000,
    max_active_recorders: 8,
  }
}

function addApplication(): void {
  if (props.service.applications.length >= 256) return
  props.service.applications.push(newApplication())
  emit('changed')
}

function removeApplication(index: number): void {
  props.service.applications.splice(index, 1)
  emit('changed')
}

function addRecorder(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  const recorders = application?.recorders
  if (!application?.live || !recorders || recorders.length >= 8) return
  recorders.push(newRecorder())
  emit('changed')
}

function recorderAddReason(application: RtmpApplicationConfig): string | undefined {
  if (!application.live) return 'The server requires live publishing for recorder-enabled applications.'
  if (application.recorders.length >= 8) return 'The server allows at most 8 recorders per application.'
  return undefined
}

function removeRecorder(applicationIndex: number, recorderIndex: number): void {
  props.service.applications[applicationIndex]?.recorders.splice(recorderIndex, 1)
  emit('changed')
}
</script>
