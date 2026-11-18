<script setup vapor lang="ts">
import { computed } from 'vue'
import StatusDot from '#design/badge/StatusDot.vue'
import type { Tone } from '#design/badge/tone'
import './styles.css'

export type FeedState = 'loading' | 'live' | 'stale' | 'unavailable' | 'paused'

const LABELS: Readonly<Record<FeedState, string>> = {
  loading: 'Loading', live: 'Live', stale: 'Stale', unavailable: 'Unavailable', paused: 'Paused',
}
const TONES: Readonly<Record<FeedState, Tone>> = {
  loading: 'neutral', live: 'ok', stale: 'warn', unavailable: 'bad', paused: 'info',
}

const props = defineProps<{ state: FeedState; age?: string }>()
const label = computed(() => LABELS[props.state])
const tone = computed(() => TONES[props.state])
</script>

<template lang="pug">
p.ox-feed
  span(role="status" :data-status="state")
    StatusDot(:tone="tone")
    |  {{ label }}
  span.ox-feed-age(v-if="age") {{ age }}
</template>
