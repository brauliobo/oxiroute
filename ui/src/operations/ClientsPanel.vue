<template lang="pug">
.ox-ops-stack
  Panel(title="Tracked clients and live connections")
    .ox-ops-buttons
      Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing...' : 'Refresh clients & connections' }}
    Notice(kind="alert")
      Banner(v-if="error" tone="bad")
        strong Client inspection unavailable.
        |  {{ error }}
    Loading(v-if="loading && !clients && !connections" label="Loading client and connection snapshots")
    template(v-if="clients")
      h3 Clients snapshot
      pre.ox-ops-detail {{ JSON.stringify(clients, null, 2) }}
    template(v-if="connections")
      h3 Active connections snapshot
      pre.ox-ops-detail {{ JSON.stringify(connections, null, 2) }}
</template>

<script setup lang="ts" vapor>
import { onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import { ApiError, fetchClients, fetchConnections } from '../api'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const clients = ref<Record<string, unknown> | null>(null)
const connections = ref<Record<string, unknown> | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

async function load(): Promise<void> {
  if (!props.token) return
  loading.value = true
  error.value = null
  try {
    const [clientData, connData] = await Promise.all([
      fetchClients(props.token),
      fetchConnections(props.token),
    ])
    clients.value = clientData
    connections.value = connData
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      emit('unauthorized')
      return
    }
    error.value = err instanceof Error ? err.message : 'Unable to fetch clients or connections'
  } finally {
    loading.value = false
  }
}

watch(() => props.token, (token) => {
  if (token) void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>
