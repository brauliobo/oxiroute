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
</script>
