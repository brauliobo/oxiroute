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
  label.field(data-field="rtmp_services[].outbound_chunk_size")
    span Outbound chunk size (bytes)
    input(type="number" min="1" max="1048576" step="1" v-model.number="service.outbound_chunk_size")
  label.field(data-field="rtmp_services[].access_log.type")
    span Session access log
    select(:value="service.access_log?.type ?? 'default'" @change="setAccessLog")
      option(value="default") Runtime default
      option(value="disabled") Disabled
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
    fieldset.object-block(data-field="rtmp_services[].applications[].fanout")
      legend Fanout bounds
      .field-grid
        label.field(data-field="rtmp_services[].applications[].fanout.max_subscribers")
          span Maximum subscribers
          input(type="number" min="1" max="1000000" step="1" v-model.number="application.fanout.max_subscribers")
        label.field(data-field="rtmp_services[].applications[].fanout.max_queue_messages_per_subscriber")
          span Queue messages per subscriber
          input(type="number" min="1" max="65536" step="1" v-model.number="application.fanout.max_queue_messages_per_subscriber")
        label.field(data-field="rtmp_services[].applications[].fanout.max_queue_bytes_per_subscriber")
          span Queue bytes per subscriber
          input(type="number" min="1" max="1073741824" step="1" v-model.number="application.fanout.max_queue_bytes_per_subscriber")
    fieldset.route-list(data-field="rtmp_services[].applications[].push_targets")
      .route-heading
        legend Push relays
        button.add-row(type="button" :disabled="!application.live || application.push_targets.length >= 16" @click="addPushTarget(applicationIndex)") + Add push target
      p.empty-list(v-if="application.push_targets.length === 0") No outbound relay is configured.
      article.route-card(v-for="(target, targetIndex) in application.push_targets" :key="targetIndex")
        header.route-card-heading
          strong Push target {{ targetIndex + 1 }}
          button.danger-link(type="button" @click="removePushTarget(applicationIndex, targetIndex)") Remove
        .field-grid
          label.field(data-field="rtmp_services[].applications[].push_targets[].host")
            span Host
            input(type="text" v-model="target.host")
          label.field(data-field="rtmp_services[].applications[].push_targets[].port")
            span Port
            input(type="number" min="1" max="65535" step="1" v-model.number="target.port")
          label.field(data-field="rtmp_services[].applications[].push_targets[].application")
            span Destination application
            input(type="text" v-model="target.application" placeholder="$name")
            small Use $name for the exact source stream name.
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
  return {
    name: '',
    live: true,
    idle_streams: true,
    push_targets: [],
    fanout: {
      max_subscribers: 1_024,
      max_queue_messages_per_subscriber: 256,
      max_queue_bytes_per_subscriber: 8_388_608,
    },
    recorders: [],
  }
}

function newRecorder(): RtmpRecorderConfig {
  return {
    name: '',
    start: 'continuous',
    root_directory: '/var/lib/oxiroute/recordings',
    suffix_template: '.flv',
    append_unix_seconds: false,
    timezone: 'utc',
    time_basis: 'segment_start',
    segment_naming: 'safe_unique',
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

function setAccessLog(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  props.service.access_log = value === 'disabled' ? { type: 'disabled' } : null
  emit('changed')
}

function addPushTarget(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  if (!application?.live || application.push_targets.length >= 16) return
  application.push_targets.push({ host: '127.0.0.1', port: 1_936, application: '$name' })
  emit('changed')
}

function removePushTarget(applicationIndex: number, targetIndex: number): void {
  props.service.applications[applicationIndex]?.push_targets.splice(targetIndex, 1)
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
