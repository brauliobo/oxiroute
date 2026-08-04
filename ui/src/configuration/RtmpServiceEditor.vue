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
fieldset.object-block(data-field="rtmp_services[].outbound_policy")
  legend Outbound relay policy
  .field-grid
    StringListField(v-model="service.outbound_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="rtmp_services[].outbound_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_domains" label="Denied domains" item-label="domain" field-path="rtmp_services[].outbound_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.deny_cidrs" :max-items="256")
    label.enable-row.compact-enable(data-field="rtmp_services[].outbound_policy.deny_private")
      input(type="checkbox" v-model="service.outbound_policy.deny_private")
      span Deny private destinations
    label.field(data-field="rtmp_services[].outbound_policy.rtmps")
      span RTMPS policy
      select(v-model="service.outbound_policy.rtmps")
        option(value="disabled") Disabled
        option(value="allowed") Allowed
        option(value="required") Required
    label.field(data-field="rtmp_services[].outbound_policy.max_chain_depth")
      span Maximum relay chain depth
      input(type="number" min="1" max="16" step="1" v-model.number="service.outbound_policy.max_chain_depth")
fieldset.object-block(data-field="rtmp_services[].callbacks")
  legend Service callbacks
  .field-grid
    label.field(data-field="rtmp_services[].callbacks.on_connect")
      span Connect callback
      input(type="text" v-model="service.callbacks.on_connect" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_disconnect")
      span Disconnect callback
      input(type="text" v-model="service.callbacks.on_disconnect" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_publish")
      span Publish callback
      input(type="text" v-model="service.callbacks.on_publish" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_publish_done")
      span Publish done callback
      input(type="text" v-model="service.callbacks.on_publish_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_play")
      span Play callback
      input(type="text" v-model="service.callbacks.on_play" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_play_done")
      span Play done callback
      input(type="text" v-model="service.callbacks.on_play_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_done")
      span Done callback
      input(type="text" v-model="service.callbacks.on_done" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.on_update")
      span Update callback
      input(type="text" v-model="service.callbacks.on_update" autocomplete="off")
    label.field(data-field="rtmp_services[].callbacks.notify_method")
      span Callback method
      select(v-model="service.callbacks.notify_method")
        option(value="post") POST
        option(value="get") GET
    label.field(data-field="rtmp_services[].callbacks.timeout_ms")
      span Callback timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.callbacks.timeout_ms")
    label.field(data-field="rtmp_services[].callbacks.notify_update_timeout_ms")
      span Update timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="service.callbacks.notify_update_timeout_ms")
    label.enable-row.compact-enable(data-field="rtmp_services[].callbacks.notify_update_strict")
      input(type="checkbox" v-model="service.callbacks.notify_update_strict")
      span Require update callback success
    label.enable-row.compact-enable(data-field="rtmp_services[].callbacks.notify_relay_redirect")
      input(type="checkbox" v-model="service.callbacks.notify_relay_redirect")
      span Notify relay redirects
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
      RtmpAccessPolicyEditor(:policy="application.publish" operation="publish")
      RtmpAccessPolicyEditor(:policy="application.play" operation="play")
      fieldset.object-block(data-field="rtmp_services[].applications[].limits")
        legend Session ceilings
        .field-grid
          label.field(data-field="rtmp_services[].applications[].limits.max_connections")
            span Maximum connections
            input(type="number" min="1" max="100000" step="1" v-model.number="application.limits.max_connections")
          label.field(data-field="rtmp_services[].applications[].limits.max_publishers")
            span Maximum publishers
            input(type="number" min="1" max="10000" step="1" v-model.number="application.limits.max_publishers")
          label.field(data-field="rtmp_services[].applications[].limits.max_viewers")
            span Maximum viewers
            input(type="number" min="1" max="1000000" step="1" v-model.number="application.limits.max_viewers")
      fieldset.object-block(data-field="rtmp_services[].applications[].relay")
        legend Relay bounds
        .field-grid
          label.field(data-field="rtmp_services[].applications[].relay.max_queue_messages")
            span Maximum relay messages
            input(type="number" min="1" max="65536" step="1" v-model.number="application.relay.max_queue_messages")
          label.field(data-field="rtmp_services[].applications[].relay.max_queue_bytes")
            span Maximum relay bytes
            input(type="number" min="1" max="1073741824" step="1" v-model.number="application.relay.max_queue_bytes")
          label.field(data-field="rtmp_services[].applications[].relay.buffer_ms")
            span Relay buffer (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.buffer_ms")
          label.field(data-field="rtmp_services[].applications[].relay.push_reconnect_ms")
            span Push reconnect (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.push_reconnect_ms")
          label.field(data-field="rtmp_services[].applications[].relay.pull_reconnect_ms")
            span Pull reconnect (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.pull_reconnect_ms")
          label.field(data-field="rtmp_services[].applications[].relay.connect_timeout_ms")
            span Relay connect timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.connect_timeout_ms")
          label.field(data-field="rtmp_services[].applications[].relay.handshake_timeout_ms")
            span Relay handshake timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.relay.handshake_timeout_ms")
      fieldset.object-block(data-field="rtmp_services[].applications[].callbacks")
        legend Application callbacks
        .field-grid
          label.field(data-field="rtmp_services[].applications[].callbacks.on_connect")
            span Connect callback
            input(type="text" v-model="application.callbacks.on_connect" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_disconnect")
            span Disconnect callback
            input(type="text" v-model="application.callbacks.on_disconnect" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_publish")
            span Publish callback
            input(type="text" v-model="application.callbacks.on_publish" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_publish_done")
            span Publish done callback
            input(type="text" v-model="application.callbacks.on_publish_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_play")
            span Play callback
            input(type="text" v-model="application.callbacks.on_play" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_play_done")
            span Play done callback
            input(type="text" v-model="application.callbacks.on_play_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_done")
            span Done callback
            input(type="text" v-model="application.callbacks.on_done" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.on_update")
            span Update callback
            input(type="text" v-model="application.callbacks.on_update" autocomplete="off")
          label.field(data-field="rtmp_services[].applications[].callbacks.notify_method")
            span Callback method
            select(v-model="application.callbacks.notify_method")
              option(value="post") POST
              option(value="get") GET
          label.field(data-field="rtmp_services[].applications[].callbacks.timeout_ms")
            span Callback timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.callbacks.timeout_ms")
          label.field(data-field="rtmp_services[].applications[].callbacks.notify_update_timeout_ms")
            span Update timeout (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.callbacks.notify_update_timeout_ms")
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].callbacks.notify_update_strict")
            input(type="checkbox" v-model="application.callbacks.notify_update_strict")
            span Require update callback success
          label.enable-row.compact-enable(data-field="rtmp_services[].applications[].callbacks.notify_relay_redirect")
            input(type="checkbox" v-model="application.callbacks.notify_relay_redirect")
            span Notify relay redirects
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
    fieldset.route-list(data-field="rtmp_services[].applications[].pull_targets")
      .route-heading
        legend Pull relays
        button.add-row(type="button" :disabled="application.pull_targets.length >= 16" @click="addPullTarget(applicationIndex)") + Add pull target
      p.empty-list(v-if="application.pull_targets.length === 0") No inbound relay is configured.
      article.route-card(v-for="(target, targetIndex) in application.pull_targets" :key="targetIndex")
        header.route-card-heading
          strong Pull target {{ targetIndex + 1 }}
          button.danger-link(type="button" @click="removePullTarget(applicationIndex, targetIndex)") Remove
        .field-grid
          label.field(data-field="rtmp_services[].applications[].pull_targets[].host")
            span Host
            input(type="text" v-model="target.host")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].port")
            span Port
            input(type="number" min="1" max="65535" step="1" v-model.number="target.port")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].application")
            span Source application
            input(type="text" v-model="target.application")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].stream_name")
            span Source stream
            input(type="text" v-model="target.stream_name")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].scheme")
            span Transport
            select(v-model="target.scheme")
              option(value="rtmp") RTMP
              option(value="rtmps") RTMPS
          label.field(data-field="rtmp_services[].applications[].pull_targets[].tc_url")
            span TC URL
            input(type="text" :value="target.tc_url ?? ''" @input="setNullableTargetField(target, 'tc_url', $event)")
          label.field(data-field="rtmp_services[].applications[].pull_targets[].flash_version")
            span Flash version
            input(type="text" :value="target.flash_version ?? ''" @input="setNullableTargetField(target, 'flash_version', $event)")
          label.enable-row(data-field="rtmp_services[].applications[].pull_targets[].credentials")
            input(type="checkbox" :checked="target.credentials !== null" @change="toggleTargetCredentials(target)")
            span Use credentials
          template(v-if="target.credentials")
            label.field(data-field="rtmp_services[].applications[].pull_targets[].credentials.username")
              span Username
              input(type="text" v-model="target.credentials.username")
            label.field(data-field="rtmp_services[].applications[].pull_targets[].credentials.secret_file")
              span Secret file
              input(type="text" v-model="target.credentials.secret_file" autocomplete="off")
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
          label.field(data-field="rtmp_services[].applications[].push_targets[].scheme")
            span Transport
            select(v-model="target.scheme")
              option(value="rtmp") RTMP
              option(value="rtmps") RTMPS
          label.field(data-field="rtmp_services[].applications[].push_targets[].stream_name")
            span Stream name override
            input(type="text" :value="target.stream_name ?? ''" @input="setNullableTargetField(target, 'stream_name', $event)")
          label.field(data-field="rtmp_services[].applications[].push_targets[].tc_url")
            span TC URL
            input(type="text" :value="target.tc_url ?? ''" @input="setNullableTargetField(target, 'tc_url', $event)")
          label.field(data-field="rtmp_services[].applications[].push_targets[].flash_version")
            span Flash version
            input(type="text" :value="target.flash_version ?? ''" @input="setNullableTargetField(target, 'flash_version', $event)")
          label.enable-row(data-field="rtmp_services[].applications[].push_targets[].credentials")
            input(type="checkbox" :checked="target.credentials !== null" @change="toggleTargetCredentials(target)")
            span Use credentials
          template(v-if="target.credentials")
            label.field(data-field="rtmp_services[].applications[].push_targets[].credentials.username")
              span Username
              input(type="text" v-model="target.credentials.username")
            label.field(data-field="rtmp_services[].applications[].push_targets[].credentials.secret_file")
              span Secret file
              input(type="text" v-model="target.credentials.secret_file" autocomplete="off")
    fieldset.object-block(data-field="rtmp_services[].applications[].vod")
      legend VOD sources
      label.enable-row
        input(type="checkbox" :checked="application.vod !== null" @change="toggleVod(application)")
        span Enable VOD playback
      template(v-if="application.vod")
        .field-grid
          label.field(data-field="rtmp_services[].applications[].vod.max_sessions")
            span Maximum VOD sessions
            input(type="number" min="1" max="100000" step="1" v-model.number="application.vod.max_sessions")
          label.field(data-field="rtmp_services[].applications[].vod.max_file_bytes")
            span Maximum VOD file bytes
            input(type="number" min="1" max="1073741824" step="1" v-model.number="application.vod.max_file_bytes")
          label.field(data-field="rtmp_services[].applications[].vod.max_duration_ms")
            span Maximum VOD duration (ms)
            input(type="number" min="1" max="86400000" step="1" v-model.number="application.vod.max_duration_ms")
        .stack-list(data-field="rtmp_services[].applications[].vod.sources")
          article.object-block(v-for="(source, sourceIndex) in application.vod.sources" :key="sourceIndex")
            .field-grid
              label.field(data-field="rtmp_services[].applications[].vod.sources[].type")
                span Source type
                select(:value="source.type" @change="changeVodSource(application, sourceIndex, $event)")
                  option(value="local") Local files
                  option(value="http") HTTP origin
              label.field(data-field="rtmp_services[].applications[].vod.sources[].name")
                span Source name
                input(type="text" v-model="source.name")
              label.field(v-if="source.type === 'local'" data-field="rtmp_services[].applications[].vod.sources[].root_directory")
                span Root directory
                input(type="text" v-model="source.root_directory")
              label.field(v-else data-field="rtmp_services[].applications[].vod.sources[].origin")
                span HTTP origin
                input(type="url" v-model="source.origin")
              button.danger-button(type="button" @click="application.vod.sources.splice(sourceIndex, 1)") Remove source
        button.secondary-button(type="button" @click="application.vod.sources.push({ type: 'local', name: '', root_directory: '' })") Add source
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
import StringListField from '../StringListField.vue'
import type {
  RtmpAccessPolicyConfig,
  RtmpApplicationConfig,
  RtmpPullTargetConfig,
  RtmpPushTargetConfig,
  RtmpRecorderConfig,
  RtmpServiceConfig,
} from '../config'
import { defaultRtmpCallback, defaultRtmpRelay } from './canonicalDefaults'
import RtmpAccessPolicyEditor from './RtmpAccessPolicyEditor.vue'
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
    publish: newAccessPolicy(),
    play: newAccessPolicy(),
    limits: {
      max_connections: 1_024,
      max_publishers: 256,
      max_viewers: 1_024,
    },
    push_targets: [],
    pull_targets: [],
    relay: defaultRtmpRelay(),
    callbacks: defaultRtmpCallback(),
    fanout: {
      max_subscribers: 1_024,
      max_queue_messages_per_subscriber: 256,
      max_queue_bytes_per_subscriber: 8_388_608,
    },
    vod: null,
    recorders: [],
  }
}

function newAccessPolicy(): RtmpAccessPolicyConfig {
  return { rules: [], token: null }
}

function newRecorder(): RtmpRecorderConfig {
  return {
    name: '',
    start: 'continuous',
    root_directory: '/var/lib/oxiroute/recordings',
    record_mask: { audio: true, video: true, keyframes: false },
    suffix_template: '.flv',
    append_unix_seconds: false,
    append: false,
    lock: false,
    max_size: null,
    max_frames: null,
    notify: false,
    timezone: 'utc',
    time_basis: 'segment_start',
    segment_naming: 'safe_unique',
    rotation_interval_ms: null,
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    shutdown_timeout_ms: 5_000,
    max_storage_bytes: null,
    max_storage_files: null,
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
  application.push_targets.push({
    host: '127.0.0.1',
    port: 1_936,
    application: '$name',
    scheme: 'rtmp',
    stream_name: null,
    tc_url: null,
    flash_version: null,
    credentials: null,
  })
  emit('changed')
}

function addPullTarget(applicationIndex: number): void {
  const application = props.service.applications[applicationIndex]
  if (!application || application.pull_targets.length >= 16) return
  application.pull_targets.push({
    host: '127.0.0.1',
    port: 1_935,
    application: 'live',
    stream_name: '',
    scheme: 'rtmp',
    tc_url: null,
    flash_version: null,
    credentials: null,
  })
  emit('changed')
}

function removePullTarget(applicationIndex: number, targetIndex: number): void {
  props.service.applications[applicationIndex]?.pull_targets.splice(targetIndex, 1)
  emit('changed')
}

function toggleTargetCredentials(target: RtmpPushTargetConfig | RtmpPullTargetConfig): void {
  target.credentials = target.credentials === null ? { username: '', secret_file: '' } : null
}

function setNullableTargetField(
  target: RtmpPushTargetConfig | RtmpPullTargetConfig,
  field: 'tc_url' | 'flash_version',
  event: Event,
): void {
  const value = (event.target as HTMLInputElement).value
  target[field] = value || null
}

function toggleVod(application: RtmpApplicationConfig): void {
  application.vod = application.vod === null
    ? { sources: [], max_sessions: 64, max_file_bytes: 67_108_864, max_duration_ms: 21_600_000 }
    : null
}

function changeVodSource(application: RtmpApplicationConfig, index: number, event: Event): void {
  const vod = application.vod
  const source = vod?.sources[index]
  if (!source) return
  const type = (event.target as HTMLSelectElement).value
  vod.sources[index] = type === 'http'
    ? { type: 'http', name: source.name, origin: '' }
    : { type: 'local', name: source.name, root_directory: '' }
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
