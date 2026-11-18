<template lang="pug">
.field-grid
  ConfigField(path="rtmp_services[].name" label="Stable name")
    input(type="text" v-model="service.name")
  ConfigField(path="rtmp_services[].outbound_chunk_size" label="Outbound chunk size (bytes)")
    input(type="number" min="1" max="1048576" step="1" v-model.number="service.outbound_chunk_size")
  ConfigField(path="rtmp_services[].access_log.type" label="Session access log")
    select(:value="service.access_log?.type ?? 'default'" @change="emit('set-access-log', $event)")
      option(value="default") Runtime default
      option(value="disabled") Disabled
ConfigGroup.object-block(path="rtmp_services[].auto_push" legend="Same-daemon worker auto-push")
  .field-grid
    ConfigField.compact-enable(path="rtmp_services[].auto_push.enabled" label="Enable worker coordination" check)
      input(type="checkbox" v-model="service.auto_push.enabled")
    ConfigField(path="rtmp_services[].auto_push.socket_dir" label="Worker socket directory")
      input(type="text" v-model="service.auto_push.socket_dir")
    ConfigField(path="rtmp_services[].auto_push.secret_file" label="Shared secret file")
      input(type="text" v-model="service.auto_push.secret_file")
    ConfigField(path="rtmp_services[].auto_push.reconnect_ms" label="Reconnect interval (ms)")
      input(type="number" min="1" max="300000" step="1" v-model.number="service.auto_push.reconnect_ms")
    ConfigField(path="rtmp_services[].auto_push.connect_timeout_ms" label="Connect timeout (ms)")
      input(type="number" min="1" max="30000" step="1" v-model.number="service.auto_push.connect_timeout_ms")
    ConfigField(path="rtmp_services[].auto_push.handshake_timeout_ms" label="Handshake timeout (ms)")
      input(type="number" min="1" max="30000" step="1" v-model.number="service.auto_push.handshake_timeout_ms")
    ConfigField(path="rtmp_services[].auto_push.max_peers" label="Maximum workers")
      input(type="number" min="1" max="64" step="1" v-model.number="service.auto_push.max_peers")
    ConfigField(path="rtmp_services[].auto_push.max_queue_messages" label="Queue messages")
      input(type="number" min="1" max="4096" step="1" v-model.number="service.auto_push.max_queue_messages")
    ConfigField(path="rtmp_services[].auto_push.max_queue_bytes" label="Queue bytes")
      input(type="number" min="1" max="67108864" step="1" v-model.number="service.auto_push.max_queue_bytes")
    ConfigField(path="rtmp_services[].auto_push.max_streams" label="Maximum streams")
      input(type="number" min="1" max="4096" step="1" v-model.number="service.auto_push.max_streams")
ConfigGroup.object-block(path="rtmp_services[].outbound_policy" legend="Outbound relay policy")
  .field-grid
    StringListField(v-model="service.outbound_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="rtmp_services[].outbound_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_domains" label="Denied domains" item-label="domain" field-path="rtmp_services[].outbound_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.deny_cidrs" :max-items="256")
    ConfigField.compact-enable(path="rtmp_services[].outbound_policy.deny_private" label="Deny private destinations" check)
      input(type="checkbox" v-model="service.outbound_policy.deny_private")
    ConfigField(path="rtmp_services[].outbound_policy.rtmps" label="RTMPS policy")
      select(v-model="service.outbound_policy.rtmps")
        option(value="disabled") Disabled
        option(value="allowed") Allowed
        option(value="required") Required
    ConfigField(path="rtmp_services[].outbound_policy.max_chain_depth" label="Maximum relay chain depth")
      input(type="number" min="1" max="16" step="1" v-model.number="service.outbound_policy.max_chain_depth")
RtmpCallbackEditor(
  :callbacks="service.callbacks"
  field-path="rtmp_services[].callbacks"
  legend="Service callbacks"
)
</template>

<script setup lang="ts" vapor>
import StringListField from '../StringListField.vue'
import type { RtmpServiceConfig } from '../config'
import RtmpCallbackEditor from './RtmpCallbackEditor.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

defineProps<{ service: RtmpServiceConfig }>()
const emit = defineEmits<{ 'set-access-log': [event: Event] }>()
</script>
