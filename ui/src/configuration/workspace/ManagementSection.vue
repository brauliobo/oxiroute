<template lang="pug">
SectionHeading(eyebrow="Optional object" title="Management" path="Config.management")
ConfigField(path="management" label="Enable the loopback management listener" check)
  input(type="checkbox" :checked="config.management !== null" @change="toggleManagement")
.field-grid(v-if="config.management")
  ConfigField(path="management.bind" label="Bind address")
    input(type="text" v-model="config.management.bind" placeholder="127.0.0.1:9080")
  ConfigField(path="management.ui_dir" label="Prebuilt UI directory")
    input(type="text" :value="config.management.ui_dir ?? ''" placeholder="/opt/oxiroute/ui/dist" @input="setUiDir")
    small Optional; empty writes null.
  ConfigField(path="management.token_file" label="Management token file")
    input(type="text" :value="config.management.token_file ?? ''" placeholder="/etc/oxiroute/management.token" @input="setTokenFile")
    small Optional; empty falls back to OXIROUTE_MANAGEMENT_TOKEN_FILE. The token itself is never stored here.
</template>

<script setup lang="ts" vapor>
import type { CanonicalConfig } from '../../config'
import { nullableInput } from './inputValues'
import SectionHeading from './SectionHeading.vue'
import ConfigField from '../fields/ConfigField.vue'

const props = defineProps<{ config: CanonicalConfig }>()

function toggleManagement(event: Event): void {
  props.config.management = (event.target as HTMLInputElement).checked
    ? { bind: '127.0.0.1:9080', ui_dir: null }
    : null
}

function setUiDir(event: Event): void {
  if (props.config.management) props.config.management.ui_dir = nullableInput(event)
}

function setTokenFile(event: Event): void {
  const management = props.config.management
  if (!management) return
  const value = nullableInput(event)
  if (value === null) delete management.token_file
  else management.token_file = value
}
</script>

<style scoped src="./controls.css"></style>
<style scoped src="./fields.css"></style>
