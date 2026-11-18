<template lang="pug">
article.route-card
  header.route-card-heading
    strong Endpoint {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove endpoint ${index + 1}`" @click="emit('remove')") Remove
  .field-grid
    ConfigField(path="upstream_pools[].servers[].endpoint.type" label="Endpoint type")
      select(:value="endpoint.type" @change="changeType")
        option(value="socket") Network socket
        option(value="dns") DNS name
        option(value="unix") Unix socket
    ConfigField(v-if="endpoint.type === 'socket'" path="upstream_pools[].servers[].endpoint.address" label="Socket address")
      input(type="text" v-model="endpoint.address" placeholder="127.0.0.1:3000")
    template(v-else-if="endpoint.type === 'dns'")
      ConfigField(path="upstream_pools[].servers[].endpoint.host" label="DNS host")
        input(type="text" v-model="endpoint.host" placeholder="backend.example.test")
      ConfigField(path="upstream_pools[].servers[].endpoint.port" label="Port")
        input(type="number" min="1" max="65535" step="1" v-model.number="endpoint.port")
    ConfigField(v-else path="upstream_pools[].servers[].endpoint.path" label="Unix socket path")
      input(type="text" v-model="endpoint.path" placeholder="/run/oxiroute/backend.sock")
</template>

<script setup lang="ts" vapor>
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
