<template lang="pug">
article.route-card
  header.route-card-heading
    strong Endpoint {{ index + 1 }}
    Button.danger-link(variant="danger" small :aria-label="`Remove endpoint ${index + 1}`" @click="emit('remove')") Remove
  .field-grid
    ConfigField(path="upstream_pools[].servers[].endpoint.type" label="Endpoint type")
      Select(
        :model-value="endpoint.type" @change="changeType"
        :options="[{ value: 'socket', label: 'Network socket' }, { value: 'dns', label: 'DNS name' }, { value: 'unix', label: 'Unix socket' }]"
      )
    ConfigField(v-if="endpoint.type === 'socket'" path="upstream_pools[].servers[].endpoint.address" label="Socket address")
      Input(v-model="endpoint.address" placeholder="127.0.0.1:3000")
    template(v-else-if="endpoint.type === 'dns'")
      ConfigField(path="upstream_pools[].servers[].endpoint.host" label="DNS host")
        Input(v-model="endpoint.host" placeholder="backend.example.test")
      ConfigField(path="upstream_pools[].servers[].endpoint.port" label="Port")
        NumberInput(min="1" max="65535" step="1" v-model="endpoint.port")
    ConfigField(v-else path="upstream_pools[].servers[].endpoint.path" label="Unix socket path")
      Input(v-model="endpoint.path" placeholder="/run/oxiroute/backend.sock")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { UpstreamEndpoint } from '../config'
import ConfigField from './fields/ConfigField.vue'

defineProps<{
  endpoint: UpstreamEndpoint
  index: number
}>()

const emit = defineEmits<{
  remove: []
  'update:endpoint': [endpoint: UpstreamEndpoint]
}>()

function changeType(event: Event): void {
  const type = (event.target as HTMLSelectElement).value
  emit(
    'update:endpoint',
    type === 'dns'
      ? { type: 'dns', host: '', port: 80 }
      : type === 'unix'
        ? { type: 'unix', path: '' }
        : { type: 'socket', address: '127.0.0.1:3000' },
  )
}
</script>
