<template lang="pug">
header.form-heading
  div
    p.eyebrow Opaque relay
    h3 {{ service.name || 'Unnamed L4 service' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove service
.field-grid
  label.field(data-field="l4_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
  label.field(data-field="l4_services[].upstream_pool")
    span Upstream pool
    select(v-model="service.upstream_pool" required)
      option(value="") Select a pool
      option(v-for="name in poolNames" :key="name" :value="name") {{ name }}
  label.field(data-field="l4_services[].connect_timeout_ms")
    span Connect timeout (ms)
    input(type="number" min="1" step="1" v-model.number="service.connect_timeout_ms")
  label.field(data-field="l4_services[].idle_timeout_ms")
    span Idle timeout (ms)
    input(type="number" min="1" step="1" v-model.number="service.idle_timeout_ms")
  label.field(data-field="l4_services[].lifetime_timeout_ms")
    span Lifetime timeout (ms)
    input(type="number" min="1" step="1" :value="service.lifetime_timeout_ms ?? ''" placeholder="No limit" @input="setLifetimeTimeout")
fieldset.object-block(data-field="l4_services[].udp")
  legend UDP policy
  label.enable-row
    input(type="checkbox" :checked="service.udp !== null" @change="toggleUdp")
    span Enable bounded UDP relay policy
  .field-grid(v-if="service.udp")
    label.field(data-field="l4_services[].udp.max_datagram_bytes")
      span Maximum datagram bytes
      input(type="number" min="1" max="65507" step="1" v-model.number="service.udp.max_datagram_bytes")
    label.field(data-field="l4_services[].udp.max_sessions")
      span Maximum sessions
      input(type="number" min="1" max="100000" step="1" v-model.number="service.udp.max_sessions")
    label.field(data-field="l4_services[].udp.max_session_bytes")
      span Maximum session bytes
      input(type="number" min="1" max="1073741824" step="1" v-model.number="service.udp.max_session_bytes")
    label.field(data-field="l4_services[].udp.max_queue_datagrams")
      span Maximum queued datagrams
      input(type="number" min="1" max="4096" step="1" v-model.number="service.udp.max_queue_datagrams")
    label.field(data-field="l4_services[].udp.max_queue_bytes")
      span Maximum queued bytes
      input(type="number" min="1" max="16777216" step="1" v-model.number="service.udp.max_queue_bytes")
</template>

<script setup lang="ts">
import type { L4ServiceConfig, UdpPolicyConfig } from '../config'

const props = defineProps<{
  service: L4ServiceConfig
  poolNames: string[]
}>()

defineEmits<{ remove: [] }>()

function setLifetimeTimeout(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.service.lifetime_timeout_ms = value === '' ? null : Number(value)
}

function toggleUdp(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  props.service.udp = enabled ? defaultUdpPolicy() : null
}

function defaultUdpPolicy(): UdpPolicyConfig {
  return {
    max_datagram_bytes: 16 * 1024,
    max_sessions: 4_096,
    max_session_bytes: 64 * 1024 * 1024,
    max_queue_datagrams: 64,
    max_queue_bytes: 1024 * 1024,
  }
}
</script>
