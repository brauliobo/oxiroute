<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow Ingress object
    h3 {{ listener.name || 'Unnamed listener' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove listener
.field-grid
  ConfigField(path="listeners[].name" label="Stable name")
    Input(v-model="listener.name")
  ConfigField(path="listeners[].protocol" label="Protocol")
    Select(
      v-model="listener.protocol" @change="normalizeReferences"
      :options="[{ value: 'http', label: 'HTTP' }, { value: 'tcp', label: 'TCP' }, { value: 'rtmp', label: 'RTMP' }, { value: 'forward_http1', label: 'Forward HTTP/1.1' }, { value: 'forward_http2', label: 'Forward HTTP/2' }, { value: 'forward_http3', label: 'Forward HTTP/3' }]"
    )
  ConfigField(path="listeners[].service" label="Service reference")
    select.ox-input(v-model="listener.service" required @change="normalizeReferences")
      option(:value="null" disabled) Select a service
      option(v-for="name in serviceNames" :key="name" :value="name") {{ name }}
  ConfigField(path="listeners[].tls_profile" label="TLS / SNI profile")
    select.ox-input(
      v-model="listener.tls_profile"
      :disabled="tlsDisabled"
      :title="tlsDisabled ? 'TLS profiles are supported only by HTTP and forward HTTP network listeners.' : undefined"
    )
      option(:value="null" :disabled="tlsRequired") None
      option(v-for="name in tlsProfileNames" :key="name" :value="name") {{ name }}
ConfigGroup.object-block(path="listeners[].bind" legend="Bind identity")
  .field-grid
    ConfigField(path="listeners[].bind.type" label="Bind type")
      select.ox-input(:value="listener.bind.type" @change="changeBind")
        option(value="socket" :disabled="listener.protocol === 'forward_http3'") Network socket
        option(value="udp" :disabled="listener.protocol !== 'forward_http3'") UDP datagram
        option(value="unix" :disabled="listener.protocol === 'forward_http3' || (listener.protocol.startsWith('forward_') && forwardService?.tls_required)") Unix socket
    ConfigField(v-if="listener.bind.type !== 'unix'" path="listeners[].bind.address" label="Socket address")
      Input(v-model="listener.bind.address" placeholder="0.0.0.0:443")
    ConfigField(v-else path="listeners[].bind.path" label="Unix socket path")
      Input(v-model="listener.bind.path" placeholder="/run/oxiroute/listener.sock")
    ConfigField(v-if="listener.bind.type === 'socket'" path="listeners[].bind.ipv6_only" label="IPv6 only (a [::] bind then leaves IPv4 on its port to another listener)" check :model-value="listener.bind.ipv6_only === true" @change="toggleIpv6Only")
    ConfigField(v-if="listener.bind.type === 'unix'" path="listeners[].bind.mode" label="Unix socket mode")
      Input(inputmode="numeric" :model-value="formatUnixMode(listener.bind.mode)" placeholder="0770" pattern="0?[0-7]{3}" @change="updateUnixMode")
NullableLimitField(
  v-model="listener.max_connections"
  :default-value="10000"
  field-path="listeners[].max_connections"
  legend="Concurrent connection limit"
  input-label="Maximum active connections"
)
ConfigGroup.object-block(path="listeners[].proxy_protocol" legend="Inbound PROXY protocol")
  Checkbox.enable-row(:model-value="listener.proxy_protocol != null" @change="toggleProxyProtocol") Accept a bounded PROXY header before application data
  .field-grid(v-if="listener.proxy_protocol")
    ConfigField(path="listeners[].proxy_protocol.version" label="Accepted version")
      Select(
        v-model="listener.proxy_protocol.version"
        :options="[{ value: 'auto', label: 'Auto-detect v1 or v2' }, { value: 'v1', label: 'PROXY v1 only' }, { value: 'v2', label: 'PROXY v2 only' }]"
      )
    ConfigField(path="listeners[].proxy_protocol.timeout_ms" label="Header timeout (ms)")
      NumberInput(min="1" max="60000" step="1" v-model="listener.proxy_protocol.timeout_ms")
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import { computed } from 'vue'

import type { ForwardProxyServiceConfig, ListenerConfig, TlsProfileConfig } from '../config'
import NullableLimitField from './NullableLimitField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

const props = defineProps<{
  listener: ListenerConfig
  httpServiceNames: string[]
  rtmpServiceNames: string[]
  l4ServiceNames: string[]
  forwardProxyServices: ForwardProxyServiceConfig[]
  tlsProfiles: TlsProfileConfig[]
}>()

const emit = defineEmits<{ remove: [] }>()

const serviceNames = computed(() => {
  switch (props.listener.protocol) {
    case 'http': return props.httpServiceNames
    case 'rtmp': return props.rtmpServiceNames
    case 'tcp': return props.l4ServiceNames
    case 'forward_http1': return forwardServiceNames('h1')
    case 'forward_http2': return forwardServiceNames('h2')
    case 'forward_http3': return forwardServiceNames('h3')
  }
})

const tlsProfileNames = computed(() => props.tlsProfiles
  .filter((profile) => {
    if (props.listener.protocol === 'forward_http1') return profile.alpn.includes('http/1.1')
    if (props.listener.protocol === 'forward_http2') return profile.alpn.includes('h2')
    if (props.listener.protocol === 'forward_http3') {
      return profile.min_version === '1.3' && profile.alpn.length === 1 && profile.alpn[0] === 'h3'
    }
    return true
  })
  .map(({ name }) => name))

const forwardService = computed(() => props.forwardProxyServices
  .find(({ name }) => name === props.listener.service))
const tlsRequired = computed(() => props.listener.protocol === 'forward_http3' ||
  (props.listener.protocol.startsWith('forward_') && props.listener.bind.type !== 'unix' &&
    forwardService.value?.tls_required === true))
const tlsDisabled = computed(
  () => !['http', 'forward_http1', 'forward_http2', 'forward_http3'].includes(props.listener.protocol) ||
    props.listener.bind.type === 'unix',
)

function forwardServiceNames(version: 'h1' | 'h2' | 'h3'): string[] {
  return props.forwardProxyServices
    .filter(({ enabled_versions }) => enabled_versions.includes(version))
    .map(({ name }) => name)
}

function normalizeReferences(): void {
  if (!serviceNames.value.includes(props.listener.service ?? '')) {
    props.listener.service = serviceNames.value[0] ?? null
  }
  if (props.listener.protocol === 'forward_http3' && props.listener.bind.type !== 'udp') {
    props.listener.bind = { type: 'udp', address: '0.0.0.0:443' }
  } else if (props.listener.protocol !== 'forward_http3' && props.listener.bind.type === 'udp') {
    props.listener.bind = { type: 'socket', address: '0.0.0.0:8080' }
  }
  if (tlsDisabled.value) props.listener.tls_profile = null
  else if (!tlsProfileNames.value.includes(props.listener.tls_profile ?? '')) {
    props.listener.tls_profile = tlsRequired.value ? tlsProfileNames.value[0] ?? null : null
  }
}

function changeBind(event: Event): void {
  const type = (event.target as HTMLSelectElement).value
  props.listener.bind = type === 'unix'
    ? { type, path: '', mode: null }
    : { type: type as 'socket' | 'udp', address: type === 'udp' ? '0.0.0.0:443' : '0.0.0.0:8080' }
  if (props.listener.bind.type === 'unix') props.listener.tls_profile = null
}

function formatUnixMode(mode: number | null): string {
  return mode === null ? '' : mode.toString(8).padStart(4, '0')
}

function updateUnixMode(event: Event): void {
  if (props.listener.bind.type !== 'unix') return
  const input = event.target as HTMLInputElement
  const mode = input.value.trim()
  props.listener.bind.mode = /^0?[0-7]{3}$/.test(mode) ? Number.parseInt(mode, 8) : null
  input.value = formatUnixMode(props.listener.bind.mode)
}

function toggleIpv6Only(event: Event): void {
  if (props.listener.bind.type !== 'socket') return
  if ((event.target as HTMLInputElement).checked) props.listener.bind.ipv6_only = true
  else delete props.listener.bind.ipv6_only
}

function toggleProxyProtocol(event: Event): void {
  props.listener.proxy_protocol = (event.target as HTMLInputElement).checked
    ? { version: 'auto', timeout_ms: 5_000 }
    : null
}
</script>
