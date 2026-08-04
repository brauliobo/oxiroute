<template lang="pug">
header.form-heading
  div
    p.eyebrow Plural SNI policy
    h3 {{ profile.name || 'Unnamed SNI profile' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove profile
.field-grid
  label.field(data-field="tls_profiles[].name")
    span Stable name
    input(type="text" v-model="profile.name")
  label.field(data-field="tls_profiles[].default_certificate")
    span Default certificate
    select(v-model="profile.default_certificate" required)
      option(value="") Select a certificate
      option(v-for="name in certificateNames" :key="name" :value="name") {{ name }}
  label.field(data-field="tls_profiles[].min_version")
    span Minimum TLS version
    select(v-model="profile.min_version")
      option(value="1.2" :disabled="profile.alpn.includes('h3')") TLS 1.2
      option(value="1.3") TLS 1.3
  label.field(data-field="tls_profiles[].alpn")
    span ALPN policy
    select(:value="profile.alpn.join(',')" @change="setAlpnPolicy")
      option(value="http/1.1") HTTP/1.1
      option(value="h2") HTTP/2 only
      option(value="h2,http/1.1") HTTP/2, then HTTP/1.1
      option(value="h3") HTTP/3 only
  label.field(data-field="tls_profiles[].policy.cipher_list")
    span OpenSSL cipher list
    input(type="text" :value="profile.policy.cipher_list ?? ''" @input="setNullableText('cipher_list', $event)")
  label.field(data-field="tls_profiles[].policy.dh_parameters_path")
    span DH parameters path
    input(type="text" :value="profile.policy.dh_parameters_path ?? ''" @input="setNullableText('dh_parameters_path', $event)")
  label.field(data-field="tls_profiles[].policy.client_auth.mode")
    span Client certificate policy
    select(v-model="profile.policy.client_auth.mode")
      option(value="disabled") Disabled
      option(value="optional") Optional
      option(value="required") Required
  label.field(data-field="tls_profiles[].policy.client_auth.ca_certificate_path")
    span Client CA bundle path
    input(type="text" :value="profile.policy.client_auth.ca_certificate_path ?? ''" :disabled="profile.policy.client_auth.mode === 'disabled'" @input="setClientCaPath")
  label.field(data-field="tls_profiles[].policy.session_cache.name")
    span Shared session cache name
    input(type="text" :value="profile.policy.session_cache?.name ?? ''" @input="setSessionCacheName")
  label.field(data-field="tls_profiles[].policy.session_cache.size_bytes")
    span Session cache bytes
    input(type="number" min="256" step="256" :disabled="profile.policy.session_cache === null" :value="profile.policy.session_cache?.size_bytes ?? 10485760" @input="setSessionCacheSize")
  label.field(data-field="tls_profiles[].policy.session_timeout_seconds")
    span Session timeout seconds
    input(type="number" min="1" step="1" :value="profile.policy.session_timeout_seconds ?? ''" @input="setSessionTimeout")
  label.checkbox-field(data-field="tls_profiles[].policy.session_tickets")
    input(type="checkbox" v-model="profile.policy.session_tickets")
    span Enable session tickets
  label.checkbox-field(data-field="tls_profiles[].policy.prefer_server_ciphers")
    input(type="checkbox" v-model="profile.policy.prefer_server_ciphers")
    span Prefer server cipher order
StringListField(
  v-model="profile.policy.client_auth.allowed_dns_names"
  label="Allowed client SANs"
  item-label="exact DNS or IP SAN"
  field-path="tls_profiles[].policy.client_auth.allowed_dns_names"
  :disabled="profile.policy.client_auth.mode === 'disabled'"
  hint="Empty accepts any SAN-bearing certificate trusted by the configured client CA bundle."
)
StringListField(
  v-model="profile.certificates"
  label="SNI certificates"
  item-label="certificate reference"
  field-path="tls_profiles[].certificates"
  :suggestions="certificateNames"
  hint="Exact SNI wins over wildcard SNI; the default must also be listed."
)
</template>

<script setup lang="ts">
import type { AlpnProtocol, TlsProfileConfig } from '../config'
import StringListField from '../StringListField.vue'

const props = defineProps<{
  profile: TlsProfileConfig
  certificateNames: string[]
}>()

defineEmits<{ remove: [] }>()

function setAlpnPolicy(event: Event): void {
  props.profile.alpn = (event.target as HTMLSelectElement).value.split(',') as AlpnProtocol[]
  if (props.profile.alpn.includes('h3')) props.profile.min_version = '1.3'
}

function setNullableText(field: 'cipher_list' | 'dh_parameters_path', event: Event): void {
  props.profile.policy[field] = (event.target as HTMLInputElement).value || null
}

function setClientCaPath(event: Event): void {
  props.profile.policy.client_auth.ca_certificate_path = (event.target as HTMLInputElement).value || null
}

function setSessionCacheName(event: Event): void {
  const name = (event.target as HTMLInputElement).value
  props.profile.policy.session_cache = name
    ? { name, size_bytes: props.profile.policy.session_cache?.size_bytes ?? 10 * 1024 * 1024 }
    : null
}

function setSessionCacheSize(event: Event): void {
  if (props.profile.policy.session_cache) {
    props.profile.policy.session_cache.size_bytes = Number((event.target as HTMLInputElement).value)
  }
}

function setSessionTimeout(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.profile.policy.session_timeout_seconds = value ? Number(value) : null
}
</script>
