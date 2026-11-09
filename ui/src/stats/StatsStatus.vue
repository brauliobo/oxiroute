<script setup vapor lang="ts">
import { computed } from 'vue'
import { formatAge, formatCount } from './format'
import { useStatsStore } from './useStatsStore'

const { status, model, ageMs } = useStatsStore()
const LABELS = {
  loading: 'Loading', live: 'Live', paused: 'Paused', stale: 'Stale', retrying: 'Retrying', error: 'Unavailable',
  unsupported: 'Unsupported version: reload the page',
}
const label = computed(() => LABELS[status.value])
const age = computed(() => (ageMs.value === undefined ? '' : `updated ${formatAge(ageMs.value)}`))
const servers = computed(() => model.value?.summary.servers)
</script>

<template lang="pug">
p.stats-status(role="status" :data-status="status") {{ label }} {{ age }}
p.stats-servers(v-if="servers") {{ formatCount(servers.up) }} of {{ formatCount(servers.total) }} servers up
</template>
