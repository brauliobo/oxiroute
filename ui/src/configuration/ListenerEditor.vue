<template lang="pug">
header.form-heading
  div
    p.eyebrow Ingress object
    h3 {{ listener.name || 'Unnamed listener' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove listener
.field-grid
  label.field(data-field="listeners[].name")
    span Stable name
    input(type="text" v-model="listener.name")
  label.field(data-field="listeners[].protocol")
    span Protocol
    select(v-model="listener.protocol" @change="normalizeReferences")
      option(value="http") HTTP
      option(value="tcp") TCP
      option(value="rtmp") RTMP
      option(value="forward_http1") Forward HTTP/1.1
      option(value="forward_http2") Forward HTTP/2
      option(value="forward_http3") Forward HTTP/3
  label.field(data-field="listeners[].service")
    span Service reference
    select(v-model="listener.service" required @change="normalizeReferences")
      option(:value="null" disabled) Select a service
      option(v-for="name in serviceNames" :key="name" :value="name") {{ name }}
  label.field(data-field="listeners[].tls_profile")
    span TLS / SNI profile
    select(
      v-model="listener.tls_profile"
      :disabled="tlsDisabled"
      :title="tlsDisabled ? 'TLS profiles are supported only by HTTP and forward HTTP network listeners.' : undefined"
    )
      option(:value="null" :disabled="tlsRequired") None
      option(v-for="name in tlsProfileNames" :key="name" :value="name") {{ name }}
fieldset.object-block(data-field="listeners[].bind")
  legend Bind identity
  .field-grid
    label.field(data-field="listeners[].bind.type")
      span Bind type
      select(:value="listener.bind.type" @change="changeBind")
        option(value="socket" :disabled="listener.protocol === 'forward_http3'") Network socket
        option(value="udp" :disabled="listener.protocol !== 'forward_http3'") UDP datagram
        option(value="unix" :disabled="listener.protocol === 'forward_http3' || (listener.protocol.startsWith('forward_') && forwardService?.tls_required)") Unix socket
    label.field(v-if="listener.bind.type !== 'unix'" data-field="listeners[].bind.address")
      span Socket address
      input(type="text" v-model="listener.bind.address" placeholder="0.0.0.0:443")
    label.field(v-else data-field="listeners[].bind.path")
      span Unix socket path
      input(type="text" v-model="listener.bind.path" placeholder="/run/oxiroute/listener.sock")
NullableLimitField(
  v-model="listener.max_connections"
  :default-value="10000"
  field-path="listeners[].max_connections"
  legend="Concurrent connection limit"
  input-label="Maximum active connections"
)
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type { ForwardProxyServiceConfig, ListenerConfig, TlsProfileConfig } from '../config'
import NullableLimitField from './NullableLimitField.vue'

const props = defineProps<{
  listener: ListenerConfig
  httpServiceNames: string[]
  rtmpServiceNames: string[]
  l4ServiceNames: string[]
  forwardProxyServices: ForwardProxyServiceConfig[]
  tlsProfiles: TlsProfileConfig[]
}>()

defineEmits<{ remove: [] }>()

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
</script>
