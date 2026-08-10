<template lang="pug">
.field-grid
  label.field(data-field="rtmp_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
  label.field(data-field="rtmp_services[].outbound_chunk_size")
    span Outbound chunk size (bytes)
    input(type="number" min="1" max="1048576" step="1" v-model.number="service.outbound_chunk_size")
  label.field(data-field="rtmp_services[].access_log.type")
    span Session access log
    select(:value="service.access_log?.type ?? 'default'" @change="$emit('set-access-log', $event)")
      option(value="default") Runtime default
      option(value="disabled") Disabled
fieldset.object-block(data-field="rtmp_services[].auto_push")
  legend Same-daemon worker auto-push
  .field-grid
    label.enable-row.compact-enable(data-field="rtmp_services[].auto_push.enabled")
      input(type="checkbox" v-model="service.auto_push.enabled")
      span Enable worker coordination
    label.field(data-field="rtmp_services[].auto_push.socket_dir")
      span Worker socket directory
      input(type="text" v-model="service.auto_push.socket_dir")
    label.field(data-field="rtmp_services[].auto_push.secret_file")
      span Shared secret file
      input(type="text" v-model="service.auto_push.secret_file")
    label.field(data-field="rtmp_services[].auto_push.reconnect_ms")
      span Reconnect interval (ms)
      input(type="number" min="1" max="300000" step="1" v-model.number="service.auto_push.reconnect_ms")
    label.field(data-field="rtmp_services[].auto_push.connect_timeout_ms")
      span Connect timeout (ms)
      input(type="number" min="1" max="30000" step="1" v-model.number="service.auto_push.connect_timeout_ms")
    label.field(data-field="rtmp_services[].auto_push.handshake_timeout_ms")
      span Handshake timeout (ms)
      input(type="number" min="1" max="30000" step="1" v-model.number="service.auto_push.handshake_timeout_ms")
    label.field(data-field="rtmp_services[].auto_push.max_peers")
      span Maximum workers
      input(type="number" min="1" max="64" step="1" v-model.number="service.auto_push.max_peers")
    label.field(data-field="rtmp_services[].auto_push.max_queue_messages")
      span Queue messages
      input(type="number" min="1" max="4096" step="1" v-model.number="service.auto_push.max_queue_messages")
    label.field(data-field="rtmp_services[].auto_push.max_queue_bytes")
      span Queue bytes
      input(type="number" min="1" max="67108864" step="1" v-model.number="service.auto_push.max_queue_bytes")
    label.field(data-field="rtmp_services[].auto_push.max_streams")
      span Maximum streams
      input(type="number" min="1" max="4096" step="1" v-model.number="service.auto_push.max_streams")
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
RtmpCallbackEditor(
  :callbacks="service.callbacks"
  field-path="rtmp_services[].callbacks"
  legend="Service callbacks"
)
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type { RtmpServiceConfig } from '../config'
import RtmpCallbackEditor from './RtmpCallbackEditor.vue'

defineProps<{ service: RtmpServiceConfig }>()
defineEmits<{ 'set-access-log': [event: Event] }>()
</script>
