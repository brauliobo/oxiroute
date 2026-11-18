<script setup vapor lang="ts">
import { computed } from 'vue'
import StatusDot from '#design/badge/StatusDot.vue'
import type { Tone } from '#design/badge/tone'
import { formatAge } from '#design/format'
import { useStatsStore } from './useStatsStore'

const { status, ageMs } = useStatsStore()
const LABELS = {
  loading: 'Loading', live: 'Live', paused: 'Paused', stale: 'Stale', retrying: 'Retrying', error: 'Unavailable',
  unsupported: 'Unsupported version',
}
const TONES: Record<keyof typeof LABELS, Tone> = {
  loading: 'neutral', live: 'ok', paused: 'info', stale: 'warn', retrying: 'warn', error: 'bad', unsupported: 'bad',
}
const label = computed(() => LABELS[status.value])
const tone = computed(() => TONES[status.value])
const age = computed(() => (ageMs.value === undefined ? '' : `updated ${formatAge(ageMs.value)}`))
</script>

<template lang="pug">
p.ox-live
  span.ox-state(role="status" :data-status="status")
    StatusDot(:tone="tone")
    | {{ label }}
  span.ox-age(v-if="age") {{ age }}
</template>
