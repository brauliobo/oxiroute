<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import type { Tone } from '#design/badge/tone'
import type { StreamState } from './types'

const STATES: Readonly<Record<StreamState, { tone: Tone; label: string }>> = {
  live: { tone: 'ok', label: 'Live' },
  connecting: { tone: 'neutral', label: 'Connecting' },
  reconnecting: { tone: 'warn', label: 'Reconnecting' },
  offline: { tone: 'bad', label: 'Offline' },
}

const props = defineProps<{ state: StreamState }>()
const current = computed(() => STATES[props.state])
</script>

<template lang="pug">
p(role="status" :data-state="state")
  Badge(:tone="current.tone" :label="current.label")
</template>
