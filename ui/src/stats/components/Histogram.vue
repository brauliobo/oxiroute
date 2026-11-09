<script setup vapor lang="ts">
import { computed } from 'vue'
import { vCssVars } from '../../cssVars'
import type { HistogramCounts } from '../percentiles'
import { histogramBars } from './histogram'
import { count, exact } from './present'
import './widgets.css'

// The lifetime latency histogram of a row, one bar per bucket with its number. Bounds are bucket upper limits.
const props = defineProps<{ histogram: HistogramCounts; bounds: readonly number[]; label: string }>()
const bars = computed(() => histogramBars(props.histogram, props.bounds))
const fill = (share: number) => ({ '--ox-w': share })
</script>

<template lang="pug">
figure.ox-hist
  figcaption {{ label }}: {{ exact(histogram.count) }} observations
  ol.ox-hist-rows(v-if="histogram.count > 0n")
    li(v-for="bar in bars" :key="bar.label")
      span.ox-hist-label {{ bar.label }}
      span.ox-hist-bar(aria-hidden="true" v-css-vars="fill(bar.share)")
      span.ox-hist-count(:title="exact(bar.count)") {{ count(bar.count) }}
  p.ox-muted(v-else) No observations yet.
</template>
