<template lang="pug">
article.route-card
  header.route-card-heading
    strong Endpoint {{ index + 1 }}
    button.danger-link(type="button" :aria-label="`Remove endpoint ${index + 1}`" @click="$emit('remove')") Remove
  .field-grid
    label.field(data-field="upstream_pools[].endpoints[].type")
      span Endpoint type
      select(:value="endpoint.type" @change="changeType")
        option(value="socket") Network socket
        option(value="dns") DNS name
        option(value="unix") Unix socket
    label.field(v-if="endpoint.type === 'socket'" data-field="upstream_pools[].endpoints[].address")
      span Socket address
      input(type="text" v-model="endpoint.address" placeholder="127.0.0.1:3000")
    template(v-else-if="endpoint.type === 'dns'")
      label.field(data-field="upstream_pools[].endpoints[].host")
        span DNS host
        input(type="text" v-model="endpoint.host" placeholder="backend.example.test")
      label.field(data-field="upstream_pools[].endpoints[].port")
        span Port
        input(type="number" min="1" max="65535" step="1" v-model.number="endpoint.port")
    label.field(v-else data-field="upstream_pools[].endpoints[].path")
      span Unix socket path
      input(type="text" v-model="endpoint.path" placeholder="/run/oxiroute/backend.sock")
</template>

<script setup lang="ts">
import type { UpstreamEndpoint } from '../config'

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
