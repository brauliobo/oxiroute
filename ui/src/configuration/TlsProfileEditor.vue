<template lang="pug">
header.form-heading
  div
    p.eyebrow Plural SNI policy
    h3 {{ profile.name || 'Unnamed SNI profile' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove profile
.field-grid
  ConfigField(path="tls_profiles[].name" label="Stable name")
    Input(v-model="profile.name")
  ConfigField(path="tls_profiles[].default_certificate" label="Default certificate")
    select.ox-input(v-model="profile.default_certificate" required)
      option(value="") Select a certificate
      option(v-for="name in certificateNames" :key="name" :value="name") {{ name }}
  ConfigField(path="tls_profiles[].min_version" label="Minimum TLS version")
    select.ox-input(v-model="profile.min_version")
      option(value="1.2" :disabled="profile.alpn.includes('h3')") TLS 1.2
      option(value="1.3") TLS 1.3
  ConfigField(path="tls_profiles[].alpn" label="ALPN policy")
    Select(
      :model-value="profile.alpn.join(',')" @change="setAlpnPolicy"
      :options="[{ value: 'http/1.1', label: 'HTTP/1.1' }, { value: 'h2', label: 'HTTP/2 only' }, { value: 'h2,http/1.1', label: 'HTTP/2, then HTTP/1.1' }, { value: 'h3', label: 'HTTP/3 only' }]"
    )
  ConfigField(path="tls_profiles[].policy.cipher_list" label="OpenSSL cipher list")
    Input(:model-value="profile.policy.cipher_list ?? ''" @input="setNullableText('cipher_list', $event)")
  ConfigField(path="tls_profiles[].policy.dh_parameters_path" label="DH parameters path")
    Input(:model-value="profile.policy.dh_parameters_path ?? ''" @input="setNullableText('dh_parameters_path', $event)")
  ConfigField(path="tls_profiles[].policy.client_auth.mode" label="Client certificate policy")
    Select(
      v-model="profile.policy.client_auth.mode"
      :options="[{ value: 'disabled', label: 'Disabled' }, { value: 'optional', label: 'Optional' }, { value: 'required', label: 'Required' }]"
    )
  ConfigField(path="tls_profiles[].policy.client_auth.ca_certificate_path" label="Client CA bundle path")
    Input(:model-value="profile.policy.client_auth.ca_certificate_path ?? ''" :disabled="profile.policy.client_auth.mode === 'disabled'" @input="setClientCaPath")
  ConfigField(path="tls_profiles[].policy.session_cache.name" label="Shared session cache name")
    Input(:model-value="profile.policy.session_cache?.name ?? ''" @input="setSessionCacheName")
  ConfigField(path="tls_profiles[].policy.session_cache.size_bytes" label="Session cache bytes")
    NumberInput(min="256" step="256" :disabled="profile.policy.session_cache === null" :model-value="profile.policy.session_cache?.size_bytes ?? 10485760" @input="setSessionCacheSize")
  ConfigField(path="tls_profiles[].policy.session_timeout_seconds" label="Session timeout seconds")
    NumberInput(min="1" step="1" :model-value="profile.policy.session_timeout_seconds ?? ''" @input="setSessionTimeout")
  ConfigField(path="tls_profiles[].policy.session_tickets" label="Enable session tickets" check v-model="profile.policy.session_tickets")
  ConfigField(path="tls_profiles[].policy.prefer_server_ciphers" label="Prefer server cipher order" check v-model="profile.policy.prefer_server_ciphers")
  ConfigField(path="tls_profiles[].policy.backend" label="TLS library")
    select.ox-input(v-model="profile.policy.backend")
      option(:value="null") Default (rustls; OpenSSL when DH parameters or a DHE cipher list need it)
      option(value="rustls") rustls (no DH parameters, no DHE cipher suites)
      option(value="openssl") OpenSSL
  ConfigField(path="tls_profiles[].policy.key_exchange_groups" label="Key exchange groups, in preference order (x25519, secp256r1, secp384r1, x25519mlkem768; empty keeps the default list)")
    Input(:model-value="(profile.policy.key_exchange_groups ?? []).join(', ')" @input="setKeyExchangeGroups")
  ConfigField(path="tls_profiles[].policy.ktls" label="Kernel TLS")
    Select(
      v-model="profile.policy.ktls"
      :options="[{ value: 'off', label: 'Off' }, { value: 'auto', label: 'Automatic (kernel TLS when available)' }]"
    )
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

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { AlpnProtocol, TlsProfileConfig } from '../config'
import StringListField from '../StringListField.vue'
import ConfigField from './fields/ConfigField.vue'

const props = defineProps<{
  profile: TlsProfileConfig
  certificateNames: string[]
}>()

const emit = defineEmits<{ remove: [] }>()

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

function setKeyExchangeGroups(event: Event): void {
  const groups = (event.target as HTMLInputElement).value.split(',').map((group) => group.trim()).filter(Boolean)
  props.profile.policy.key_exchange_groups = groups.length ? groups : null
}

function setSessionTimeout(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.profile.policy.session_timeout_seconds = value ? Number(value) : null
}
</script>
