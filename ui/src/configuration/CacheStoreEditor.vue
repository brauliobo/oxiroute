<template lang="pug">
header.form-heading
  div
    p.eyebrow Bounded cache storage
    h3 {{ store.name || 'Unnamed cache store' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove cache store
.field-grid
  label.field(data-field="cache_stores[].name")
    span Stable name
    input(type="text" v-model="store.name" required)
  label.field(data-field="cache_stores[].type")
    span Store type
    select(:value="store.type" @change="changeType")
      option(value="memory") Memory
      option(value="disk") Disk
  label.field.field-wide(v-if="store.type === 'disk'" data-field="cache_stores[].root_directory")
    span Disk root directory
    input(type="text" v-model="store.root_directory" required placeholder="/var/cache/oxiroute")
    small Authenticated configuration only; disk roots are suppressed from topology views.

fieldset.object-block
  legend Capacity limits
  .field-grid
    label.field(data-field="cache_stores[].max_bytes")
      span Maximum store bytes
      input(type="number" min="1" max="1125899906842624" step="1" v-model.number="store.max_bytes")
    label.field(v-if="store.type === 'memory'" data-field="cache_stores[].max_entries")
      span Maximum entries
      input(type="number" min="1" max="10000000" step="1" v-model.number="store.max_entries")
    label.field(v-else data-field="cache_stores[].max_files")
      span Maximum files
      input(type="number" min="1" max="10000000" step="1" v-model.number="store.max_files")
    label.field(data-field="cache_stores[].max_object_bytes")
      span Maximum object bytes
      input(type="number" min="1" :max="Math.min(1073741824, store.max_bytes)" step="1" v-model.number="store.max_object_bytes")
    label.field(data-field="cache_stores[].max_header_bytes")
      span Maximum header bytes
      input(type="number" min="1" :max="Math.min(1048576, store.max_object_bytes)" step="1" v-model.number="store.max_header_bytes")
    label.field(data-field="cache_stores[].max_key_bytes")
      span Maximum key bytes
      input(type="number" min="1" max="16384" step="1" v-model.number="store.max_key_bytes")
    label.field(data-field="cache_stores[].max_tag_bytes")
      span Maximum tag bytes
      input(type="number" min="1" max="1024" step="1" v-model.number="store.max_tag_bytes")
    label.field(data-field="cache_stores[].max_tags_per_object")
      span Maximum tags per object
      input(type="number" min="1" max="256" step="1" v-model.number="store.max_tags_per_object")
    label.field(data-field="cache_stores[].max_in_flight_fills")
      span Maximum in-flight fills
      input(type="number" min="1" max="65536" step="1" v-model.number="store.max_in_flight_fills")
    label.field(data-field="cache_stores[].max_followers_per_fill")
      span Maximum followers per fill
      input(type="number" min="1" max="4096" step="1" v-model.number="store.max_followers_per_fill")
</template>

<script setup lang="ts">
import type { CacheStoreConfig } from '../config'
import { defaultCacheStore } from './canonicalDefaults'

const props = defineProps<{ store: CacheStoreConfig }>()
const emit = defineEmits<{
  remove: []
  replace: [store: CacheStoreConfig]
}>()

function changeType(event: Event): void {
  const replacement = defaultCacheStore((event.target as HTMLSelectElement).value as CacheStoreConfig['type'])
  replacement.name = props.store.name
  emit('replace', replacement)
}
</script>
