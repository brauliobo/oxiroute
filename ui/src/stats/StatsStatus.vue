<script setup vapor lang="ts">
import { computed } from 'vue'
import { formatAge } from './format'
import { useStatsStore } from './useStatsStore'

// The status word is the live region; the age ticks every second and stays outside it.
const { status, ageMs } = useStatsStore()
const LABELS = {
  loading: 'Loading', live: 'Live', paused: 'Paused', stale: 'Stale', retrying: 'Retrying', error: 'Unavailable',
  unsupported: 'Unsupported version',
}
const label = computed(() => LABELS[status.value])
const age = computed(() => (ageMs.value === undefined ? '' : `updated ${formatAge(ageMs.value)}`))
</script>

<template lang="pug">
p.ox-live
  span.ox-state(role="status" :data-status="status") {{ label }}
  span.ox-age(v-if="age") {{ age }}
</template>
