<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow Bounded cache storage
    h3 {{ store.name || 'Unnamed cache store' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove cache store
.field-grid
  ConfigField(path="cache_stores[].name" label="Stable name")
    Input(v-model="store.name" required)
  ConfigField(path="cache_stores[].type" label="Store type")
    Select(
      :model-value="store.type" @change="changeType"
      :options="[{ value: 'memory', label: 'Memory' }, { value: 'disk', label: 'Disk' }]"
    )
  ConfigField.field-wide(v-if="store.type === 'disk'" path="cache_stores[].root_directory" label="Disk root directory")
    Input(v-model="store.root_directory" required placeholder="/var/cache/oxiroute")
    small Authenticated configuration only; disk roots are suppressed from topology views.

fieldset.object-block
  legend Capacity limits
  .field-grid
    ConfigField(path="cache_stores[].max_bytes" label="Maximum store bytes")
      NumberInput(min="1" max="1125899906842624" step="1" v-model="store.max_bytes")
    ConfigField(v-if="store.type === 'memory'" path="cache_stores[].max_entries" label="Maximum entries")
      NumberInput(min="1" max="10000000" step="1" v-model="store.max_entries")
    ConfigField(v-else path="cache_stores[].max_files" label="Maximum files")
      NumberInput(min="1" max="10000000" step="1" v-model="store.max_files")
    ConfigField(path="cache_stores[].max_object_bytes" label="Maximum object bytes")
      NumberInput(min="1" :max="Math.min(1073741824, store.max_bytes)" step="1" v-model="store.max_object_bytes")
    ConfigField(path="cache_stores[].max_header_bytes" label="Maximum header bytes")
      NumberInput(min="1" :max="Math.min(1048576, store.max_object_bytes)" step="1" v-model="store.max_header_bytes")
    ConfigField(path="cache_stores[].max_key_bytes" label="Maximum key bytes")
      NumberInput(min="1" max="16384" step="1" v-model="store.max_key_bytes")
    ConfigField(path="cache_stores[].max_tag_bytes" label="Maximum tag bytes")
      NumberInput(min="1" max="1024" step="1" v-model="store.max_tag_bytes")
    ConfigField(path="cache_stores[].max_tags_per_object" label="Maximum tags per object")
      NumberInput(min="1" max="256" step="1" v-model="store.max_tags_per_object")
    ConfigField(path="cache_stores[].max_in_flight_fills" label="Maximum in-flight fills")
      NumberInput(min="1" max="65536" step="1" v-model="store.max_in_flight_fills")
    ConfigField(path="cache_stores[].max_followers_per_fill" label="Maximum followers per fill")
      NumberInput(min="1" max="4096" step="1" v-model="store.max_followers_per_fill")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { CacheStoreConfig } from '../config'
import { defaultCacheStore } from './canonicalDefaults'
import ConfigField from './fields/ConfigField.vue'

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
