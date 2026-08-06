<template lang="pug">
header.form-heading
  div
    p.eyebrow Certificate identity
    h3 {{ certificate.name || 'Unnamed certificate' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove certificate
.field-grid
  label.field(data-field="certificates[].name")
    span Stable name
    input(type="text" v-model="certificate.name")
  StringListField(
    v-model="certificate.dns_names"
    label="Declared DNS names"
    item-label="DNS name"
    field-path="certificates[].dns_names"
    hint="Exact and one-label wildcard names are supported."
  )
fieldset.object-block(data-field="certificates[].source")
  legend Certificate source
  label.field(data-field="certificates[].source.type")
    span Source type
    select(:value="certificate.source.type" @change="changeSource")
      option(value="files") Direct files
      option(value="certbot") Certbot lineage
      option(value="acme_managed") Managed ACME
      option(value="self_signed_development") Development self-signed
  .field-grid(v-if="certificate.source.type === 'files'")
    label.field(data-field="certificates[].source.certificate_chain_path")
      span Certificate chain path
      input(type="text" v-model="certificate.source.certificate_chain_path")
    label.field(data-field="certificates[].source.private_key_path")
      span Private key path
      input(type="text" v-model="certificate.source.private_key_path")
  .field-grid(v-else-if="certificate.source.type === 'certbot'")
    label.field(data-field="certificates[].source.live_directory_path")
      span Live directory path
      input(type="text" v-model="certificate.source.live_directory_path")
    label.field(data-field="certificates[].source.archive_directory_path")
      span Archive directory path
      input(type="text" v-model="certificate.source.archive_directory_path")
  .field-grid(v-else-if="certificate.source.type === 'acme_managed'")
    label.field(data-field="certificates[].source.directory_url")
      span ACME directory URL
      input(type="url" v-model="certificate.source.directory_url")
    label.field(data-field="certificates[].source.state_root")
      span State root
      input(type="text" v-model="certificate.source.state_root")
    StringListField(
      v-model="certificate.source.contacts"
      label="Account contacts"
      item-label="mailto contact"
      field-path="certificates[].source.contacts"
    )
    label.field(data-field="certificates[].source.terms_agreed")
      span Terms agreed
      input(type="checkbox" v-model="certificate.source.terms_agreed")
    label.field(data-field="certificates[].source.challenge")
      span Challenge
      select(
        :value="certificate.source.challenge"
        :aria-describedby="certificate.source.challenge === 'tls_alpn01' ? 'tls-alpn-deployment-note' : undefined"
        @change="changeAcmeChallenge"
      )
        option(value="http01") HTTP-01
        option(value="dns01") DNS-01
        option(value="tls_alpn01") TLS-ALPN-01
      small#tls-alpn-deployment-note.challenge-note(v-if="certificate.source.challenge === 'tls_alpn01'")
        | TLS-ALPN-01 requires a deployed listener reachable on public TCP port 443.
        | Selecting it records the challenge choice only; it does not create or deploy that listener.
    label.field(data-field="certificates[].source.key_type")
      span Leaf key type
      select(v-model="certificate.source.key_type")
        option(value="ecdsa_p256") ECDSA P-256
        option(value="rsa_2048") RSA 2048
    StringListField(
      v-model="certificate.source.allowed_dns_suffixes"
      label="Allowed DNS suffixes"
      item-label="DNS suffix"
      field-path="certificates[].source.allowed_dns_suffixes"
    )
    label.field(data-field="certificates[].source.retained_revisions")
      span Retained revisions
      input(type="number" min="1" max="32" step="1" v-model.number="certificate.source.retained_revisions")
    label.field(data-field="certificates[].source.retention_days")
      span Retention days
      input(type="number" min="1" max="3650" step="1" v-model.number="certificate.source.retention_days")
    .field-grid(v-if="certificate.source.challenge === 'dns01' && certificate.source.dns01")
      label.field(data-field="certificates[].source.dns01.provider")
        span DNS provider
        input(type="text" v-model="certificate.source.dns01.provider")
      label.field(data-field="certificates[].source.dns01.credential_file")
        span Credential file
        input(type="text" v-model="certificate.source.dns01.credential_file")
      label.field(data-field="certificates[].source.dns01.timeout_seconds")
        span Provider timeout (seconds)
        input(type="number" min="1" max="600" step="1" v-model.number="certificate.source.dns01.timeout_seconds")
  .field-grid(v-else)
    label.field(data-field="certificates[].source.validity_days")
      span Validity days
      input(type="number" min="1" max="30" step="1" :value="certificate.source.validity_days" @input="setSelfSignedValidity")
    label.field(data-field="certificates[].source.key_type")
      span Key type
      select(:value="certificate.source.key_type" @change="setSelfSignedKeyType")
        option(value="ecdsa_p256") ECDSA P-256
        option(value="rsa_2048") RSA 2048
</template>

<script setup lang="ts">
import type { CertificateConfig, SelfSignedKeyType } from '../config'
import StringListField from '../StringListField.vue'

const props = defineProps<{ certificate: CertificateConfig }>()
defineEmits<{ remove: [] }>()

function changeSource(event: Event): void {
  const sourceType = (event.target as HTMLSelectElement).value
  props.certificate.source = sourceType === 'certbot'
    ? { type: 'certbot', live_directory_path: '', archive_directory_path: '' }
    : sourceType === 'acme_managed'
      ? {
          type: 'acme_managed',
          directory_url: 'https://acme-v02.api.letsencrypt.org/directory',
          state_root: '',
          contacts: [],
          terms_agreed: false,
          challenge: 'http01',
          key_type: 'ecdsa_p256',
          allowed_dns_suffixes: [],
          retained_revisions: 3,
          retention_days: 30,
          dns01: null,
        }
    : sourceType === 'self_signed_development'
      ? { type: 'self_signed_development', validity_days: 7, key_type: 'ecdsa_p256' }
      : { type: 'files', certificate_chain_path: '', private_key_path: '' }
}

function changeAcmeChallenge(event: Event): void {
  if (props.certificate.source.type !== 'acme_managed') return
  const challenge = (event.target as HTMLSelectElement).value as typeof props.certificate.source.challenge
  props.certificate.source.challenge = challenge
  if (challenge === 'dns01' && props.certificate.source.dns01 === null) {
    props.certificate.source.dns01 = {
      provider: '',
      credential_file: '',
      timeout_seconds: 300,
    }
  } else if (challenge !== 'dns01') {
    props.certificate.source.dns01 = null
  }
}

function setSelfSignedValidity(event: Event): void {
  if (props.certificate.source.type === 'self_signed_development') {
    props.certificate.source.validity_days = Number((event.target as HTMLInputElement).value)
  }
}

function setSelfSignedKeyType(event: Event): void {
  if (props.certificate.source.type === 'self_signed_development') {
    props.certificate.source.key_type = (event.target as HTMLSelectElement).value as SelfSignedKeyType
  }
}
</script>
