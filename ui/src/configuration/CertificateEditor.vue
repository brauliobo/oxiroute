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
  .field-grid(v-if="certificate.source.type === 'files'")
    label.field(data-field="certificates[].source.certificate_chain_path")
      span Certificate chain path
      input(type="text" v-model="certificate.source.certificate_chain_path")
    label.field(data-field="certificates[].source.private_key_path")
      span Private key path
      input(type="text" v-model="certificate.source.private_key_path")
  .field-grid(v-else)
    label.field(data-field="certificates[].source.live_directory_path")
      span Live directory path
      input(type="text" v-model="certificate.source.live_directory_path")
    label.field(data-field="certificates[].source.archive_directory_path")
      span Archive directory path
      input(type="text" v-model="certificate.source.archive_directory_path")
</template>

<script setup lang="ts">
import type { CertificateConfig } from '../config'
import StringListField from '../StringListField.vue'

const props = defineProps<{ certificate: CertificateConfig }>()
defineEmits<{ remove: [] }>()

function changeSource(event: Event): void {
  props.certificate.source = (event.target as HTMLSelectElement).value === 'certbot'
    ? { type: 'certbot', live_directory_path: '', archive_directory_path: '' }
    : { type: 'files', certificate_chain_path: '', private_key_path: '' }
}
</script>
