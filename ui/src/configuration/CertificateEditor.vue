<template lang="pug">
header.form-heading
  div
    p.eyebrow Certificate identity
    h3 {{ certificate.name || 'Unnamed certificate' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove certificate
.field-grid
  ConfigField(path="certificates[].name" label="Stable name")
    Input(v-model="certificate.name")
  StringListField(
    v-model="certificate.dns_names"
    label="Declared DNS names"
    item-label="DNS name"
    field-path="certificates[].dns_names"
    hint="Exact and one-label wildcard names are supported."
  )
ConfigGroup.object-block(path="certificates[].source" legend="Certificate source")
  ConfigField(path="certificates[].source.type" label="Source type")
    Select(
      :model-value="certificate.source.type" @change="changeSource"
      :options="[{ value: 'files', label: 'Direct files' }, { value: 'certbot', label: 'Certbot lineage' }, { value: 'acme_managed', label: 'Managed ACME' }, { value: 'self_signed_development', label: 'Development self-signed' }]"
    )
  .field-grid(v-if="certificate.source.type === 'files'")
    ConfigField(path="certificates[].source.certificate_chain_path" label="Certificate chain path")
      Input(v-model="certificate.source.certificate_chain_path")
    ConfigField(path="certificates[].source.private_key_path" label="Private key path")
      Input(v-model="certificate.source.private_key_path")
  .field-grid(v-else-if="certificate.source.type === 'certbot'")
    ConfigField(path="certificates[].source.live_directory_path" label="Live directory path")
      Input(v-model="certificate.source.live_directory_path")
    ConfigField(path="certificates[].source.archive_directory_path" label="Archive directory path")
      Input(v-model="certificate.source.archive_directory_path")
  .field-grid(v-else-if="certificate.source.type === 'acme_managed'")
    ConfigField(path="certificates[].source.directory_url" label="ACME directory URL")
      input.ox-input(type="url" v-model="certificate.source.directory_url")
    ConfigField(path="certificates[].source.state_root" label="State root")
      Input(v-model="certificate.source.state_root")
    StringListField(
      v-model="certificate.source.contacts"
      label="Account contacts"
      item-label="mailto contact"
      field-path="certificates[].source.contacts"
    )
    ConfigField(path="certificates[].source.terms_agreed" label="Terms agreed")
      input(type="checkbox" v-model="certificate.source.terms_agreed")
    ConfigField(path="certificates[].source.challenge" label="Challenge")
      Select(
        :model-value="certificate.source.challenge" :aria-describedby="certificate.source.challenge === 'tls_alpn01' ? 'tls-alpn-deployment-note' : undefined" @change="changeAcmeChallenge"
        :options="[{ value: 'http01', label: 'HTTP-01' }, { value: 'dns01', label: 'DNS-01' }, { value: 'tls_alpn01', label: 'TLS-ALPN-01' }]"
      )
      small#tls-alpn-deployment-note.challenge-note(v-if="certificate.source.challenge === 'tls_alpn01'")
        | TLS-ALPN-01 requires a deployed listener reachable on public TCP port 443.
        | Selecting it records the challenge choice only; it does not create or deploy that listener by itself. Use the draft assistant below to prepare it.
    ConfigField(path="certificates[].source.key_type" label="Leaf key type")
      Select(
        v-model="certificate.source.key_type"
        :options="[{ value: 'ecdsa_p256', label: 'ECDSA P-256' }, { value: 'rsa_2048', label: 'RSA 2048' }]"
      )
    StringListField(
      v-model="certificate.source.allowed_dns_suffixes"
      label="Allowed DNS suffixes"
      item-label="DNS suffix"
      field-path="certificates[].source.allowed_dns_suffixes"
    )
    ConfigField(path="certificates[].source.retained_revisions" label="Retained revisions")
      NumberInput(min="1" max="32" step="1" v-model="certificate.source.retained_revisions")
    ConfigField(path="certificates[].source.retention_days" label="Retention days")
      NumberInput(min="1" max="3650" step="1" v-model="certificate.source.retention_days")
    .field-grid(v-if="certificate.source.challenge === 'dns01' && certificate.source.dns01")
      ConfigField(path="certificates[].source.dns01.provider" label="DNS provider")
        Input(v-model="certificate.source.dns01.provider")
      ConfigField(path="certificates[].source.dns01.credential_file" label="Credential file")
        Input(v-model="certificate.source.dns01.credential_file")
      ConfigField(path="certificates[].source.dns01.timeout_seconds" label="Provider timeout (seconds)")
        NumberInput(min="1" max="600" step="1" v-model="certificate.source.dns01.timeout_seconds")
    .tls-alpn-deployment(v-if="certificate.source.challenge === 'tls_alpn01'")
      strong Prepare the TLS-ALPN listener in this draft
      p The assistant reuses a compatible TCP/TLS listener on port 443 or adds an isolated 404 HTTP service, TLS profile, and listener. Nothing is deployed until validation, review, and the revision-checked save.
      Button.add-row(data-action="prepare-tls-alpn-listener" @click="emit('prepare-tls-alpn')") Prepare TLS-ALPN listener
  .field-grid(v-else)
    ConfigField(path="certificates[].source.validity_days" label="Validity days")
      NumberInput(min="1" max="30" step="1" :model-value="certificate.source.validity_days" @input="setSelfSignedValidity")
    ConfigField(path="certificates[].source.key_type" label="Key type")
      Select(
        :model-value="certificate.source.key_type" @change="setSelfSignedKeyType"
        :options="[{ value: 'ecdsa_p256', label: 'ECDSA P-256' }, { value: 'rsa_2048', label: 'RSA 2048' }]"
      )
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { CertificateConfig, SelfSignedKeyType } from '../config'
import StringListField from '../StringListField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

const props = defineProps<{ certificate: CertificateConfig }>()
const emit = defineEmits<{
  remove: []
  'prepare-tls-alpn': []
}>()

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
