<template lang="pug">
header.form-heading
  div
    p.eyebrow Opaque relay
    h3 {{ service.name || 'Unnamed L4 service' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove service
.field-grid
  label.field(data-field="l4_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
  label.field(data-field="l4_services[].upstream_pool")
    span Upstream pool
    select(v-model="service.upstream_pool" required)
      option(value="") Select a pool
      option(v-for="name in poolNames" :key="name" :value="name") {{ name }}
  label.field(data-field="l4_services[].connect_timeout_ms")
    span Connect timeout (ms)
    input(type="number" min="1" step="1" v-model.number="service.connect_timeout_ms")
  label.field(data-field="l4_services[].idle_timeout_ms")
    span Idle timeout (ms)
    input(type="number" min="1" step="1" v-model.number="service.idle_timeout_ms")
  label.field(data-field="l4_services[].lifetime_timeout_ms")
    span Lifetime timeout (ms)
    input(type="number" min="1" step="1" :value="service.lifetime_timeout_ms ?? ''" placeholder="No limit" @input="setLifetimeTimeout")
</template>

<script setup lang="ts">
import type { L4ServiceConfig } from '../config'

const props = defineProps<{
  service: L4ServiceConfig
  poolNames: string[]
}>()

defineEmits<{ remove: [] }>()

function setLifetimeTimeout(event: Event): void {
  const value = (event.target as HTMLInputElement).value
  props.service.lifetime_timeout_ms = value === '' ? null : Number(value)
}
</script>
