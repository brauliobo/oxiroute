<template lang="pug">
.field-grid
  ConfigField(path="rtmp_services[].name" label="Stable name")
    Input(v-model="service.name")
  ConfigField(path="rtmp_services[].outbound_chunk_size" label="Outbound chunk size (bytes)")
    NumberInput(min="1" max="1048576" step="1" v-model="service.outbound_chunk_size")
  ConfigField(path="rtmp_services[].access_log.type" label="Session access log")
    Select(
      :model-value="service.access_log?.type ?? 'default'" @change="emit('set-access-log', $event)"
      :options="[{ value: 'default', label: 'Runtime default' }, { value: 'disabled', label: 'Disabled' }]"
    )
ConfigGroup.object-block(path="rtmp_services[].auto_push" legend="Same-daemon worker auto-push")
  .field-grid
    ConfigField.compact-enable(path="rtmp_services[].auto_push.enabled" label="Enable worker coordination" check v-model="service.auto_push.enabled")
    ConfigField(path="rtmp_services[].auto_push.socket_dir" label="Worker socket directory")
      Input(:model-value="service.auto_push.socket_dir ?? ''" @update:model-value="service.auto_push.socket_dir = $event")
    ConfigField(path="rtmp_services[].auto_push.secret_file" label="Shared secret file")
      Input(:model-value="service.auto_push.secret_file ?? ''" @update:model-value="service.auto_push.secret_file = $event")
    ConfigField(path="rtmp_services[].auto_push.reconnect_ms" label="Reconnect interval (ms)")
      NumberInput(min="1" max="300000" step="1" v-model="service.auto_push.reconnect_ms")
    ConfigField(path="rtmp_services[].auto_push.connect_timeout_ms" label="Connect timeout (ms)")
      NumberInput(min="1" max="30000" step="1" v-model="service.auto_push.connect_timeout_ms")
    ConfigField(path="rtmp_services[].auto_push.handshake_timeout_ms" label="Handshake timeout (ms)")
      NumberInput(min="1" max="30000" step="1" v-model="service.auto_push.handshake_timeout_ms")
    ConfigField(path="rtmp_services[].auto_push.max_peers" label="Maximum workers")
      NumberInput(min="1" max="64" step="1" v-model="service.auto_push.max_peers")
    ConfigField(path="rtmp_services[].auto_push.max_queue_messages" label="Queue messages")
      NumberInput(min="1" max="4096" step="1" v-model="service.auto_push.max_queue_messages")
    ConfigField(path="rtmp_services[].auto_push.max_queue_bytes" label="Queue bytes")
      NumberInput(min="1" max="67108864" step="1" v-model="service.auto_push.max_queue_bytes")
    ConfigField(path="rtmp_services[].auto_push.max_streams" label="Maximum streams")
      NumberInput(min="1" max="4096" step="1" v-model="service.auto_push.max_streams")
ConfigGroup.object-block(path="rtmp_services[].outbound_policy" legend="Outbound relay policy")
  .field-grid
    StringListField(v-model="service.outbound_policy.allow_domains" label="Allowed domains" item-label="domain" field-path="rtmp_services[].outbound_policy.allow_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_domains" label="Denied domains" item-label="domain" field-path="rtmp_services[].outbound_policy.deny_domains" :max-items="256")
    StringListField(v-model="service.outbound_policy.allow_cidrs" label="Allowed CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.allow_cidrs" :max-items="256")
    StringListField(v-model="service.outbound_policy.deny_cidrs" label="Denied CIDRs" item-label="CIDR" field-path="rtmp_services[].outbound_policy.deny_cidrs" :max-items="256")
    ConfigField.compact-enable(path="rtmp_services[].outbound_policy.deny_private" label="Deny private destinations" check v-model="service.outbound_policy.deny_private")
    ConfigField(path="rtmp_services[].outbound_policy.rtmps" label="RTMPS policy")
      Select(
        v-model="service.outbound_policy.rtmps"
        :options="[{ value: 'disabled', label: 'Disabled' }, { value: 'allowed', label: 'Allowed' }, { value: 'required', label: 'Required' }]"
      )
    ConfigField(path="rtmp_services[].outbound_policy.max_chain_depth" label="Maximum relay chain depth")
      NumberInput(min="1" max="16" step="1" v-model="service.outbound_policy.max_chain_depth")
RtmpCallbackEditor(
  :callbacks="service.callbacks"
  field-path="rtmp_services[].callbacks"
  legend="Service callbacks"
)
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import StringListField from '../StringListField.vue'
import type { RtmpServiceConfig } from '../config'
import RtmpCallbackEditor from './RtmpCallbackEditor.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

defineProps<{ service: RtmpServiceConfig }>()
const emit = defineEmits<{ 'set-access-log': [event: Event] }>()
</script>
