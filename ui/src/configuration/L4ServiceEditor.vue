<template lang="pug">
header.form-heading
  div
    p.eyebrow Opaque relay
    h3 {{ service.name || 'Unnamed L4 service' }}
  button.danger-button(type="button" @click="emit('remove')") Remove service
.field-grid
  ConfigField(path="l4_services[].name" label="Stable name")
    input(type="text" v-model="service.name")
  ConfigField(path="l4_services[].upstream_pool" label="Upstream pool")
    select(v-model="service.upstream_pool" required)
      option(value="") Select a pool
      option(v-for="name in poolNames" :key="name" :value="name") {{ name }}
  ConfigField(path="l4_services[].connect_timeout_ms" label="Connect timeout (ms)")
    input(type="number" min="1" step="1" v-model.number="service.connect_timeout_ms")
  ConfigField(path="l4_services[].idle_timeout_ms" label="Idle timeout (ms)")
    input(type="number" min="1" step="1" v-model.number="service.idle_timeout_ms")
  ConfigField(path="l4_services[].lifetime_timeout_ms" label="Lifetime timeout (ms)")
    input(type="number" min="1" step="1" :value="service.lifetime_timeout_ms ?? ''" placeholder="No limit" @input="setLifetimeTimeout")
ConfigGroup.object-block(path="l4_services[].proxy_protocol" legend="Outbound PROXY protocol")
  label.enable-row
    input(type="checkbox" :checked="service.proxy_protocol != null" @change="toggleProxyProtocol")
    span Send the selected client address upstream
  .field-grid(v-if="service.proxy_protocol")
    ConfigField(path="l4_services[].proxy_protocol.version" label="Version")
      select(v-model="service.proxy_protocol.version")
        option(value="v1") PROXY v1
        option(value="v2") PROXY v2
    ConfigField(path="l4_services[].proxy_protocol.timeout_ms" label="Header timeout (ms)")
      input(type="number" min="1" max="60000" step="1" v-model.number="service.proxy_protocol.timeout_ms")
ConfigGroup.object-block(path="l4_services[].udp" legend="UDP policy")
  label.enable-row
    input(type="checkbox" :checked="service.udp !== null" @change="toggleUdp")
    span Enable bounded UDP relay policy
  .field-grid(v-if="service.udp")
    ConfigField(path="l4_services[].udp.max_datagram_bytes" label="Maximum datagram bytes")
      input(type="number" min="1" max="65507" step="1" v-model.number="service.udp.max_datagram_bytes")
    ConfigField(path="l4_services[].udp.max_sessions" label="Maximum sessions")
      input(type="number" min="1" max="100000" step="1" v-model.number="service.udp.max_sessions")
    ConfigField(path="l4_services[].udp.max_session_bytes" label="Maximum session bytes")
      input(type="number" min="1" max="1073741824" step="1" v-model.number="service.udp.max_session_bytes")
    ConfigField(path="l4_services[].udp.max_queue_datagrams" label="Maximum queued datagrams")
      input(type="number" min="1" max="4096" step="1" v-model.number="service.udp.max_queue_datagrams")
    ConfigField(path="l4_services[].udp.max_queue_bytes" label="Maximum queued bytes")
      input(type="number" min="1" max="16777216" step="1" v-model.number="service.udp.max_queue_bytes")
</template>

<script setup lang="ts" vapor>
import type { L4ServiceConfig, UdpPolicyConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

const props = defineProps<{
  service: L4ServiceConfig
  poolNames: string[]
}>()

const emit = defineEmits<{ remove: [] }>()

function setLifetimeTimeout(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.service.lifetime_timeout_ms = value === '' ? null : Number(value)
}

function toggleUdp(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  props.service.udp = enabled ? defaultUdpPolicy() : null
}

function toggleProxyProtocol(event: Event): void {
  props.service.proxy_protocol = (event.target as HTMLInputElement).checked
    ? { version: 'v2', timeout_ms: 5_000 }
    : null
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
