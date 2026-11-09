<script setup vapor lang="ts">
import { computed } from 'vue'
import type { LatencySummary } from '../percentiles'
import { quantileText, quantileTitle } from './present'
import './widgets.css'

// p50, p90 and p99 of a lifetime histogram; each is the upper bound of its bucket (see the tooltip).
const props = defineProps<{ summary?: LatencySummary }>()
const items = computed(() => (['p50', 'p90', 'p99'] as const).map((name) => ({ name, value: props.summary?.[name] })))
</script>

<template lang="pug">
ul.ox-lat
  li(v-for="item in items" :key="item.name" :title="quantileTitle(item.value)")
    span.ox-lat-name {{ item.name }}
    |  {{ quantileText(item.value) }}
</template>
