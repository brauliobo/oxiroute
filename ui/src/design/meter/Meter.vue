<script setup vapor lang="ts">
import { computed } from 'vue'
import { vCssVars } from '../directives/cssVars'
import type { Tone } from '../badge/tone'
import './styles.css'

const props = withDefaults(defineProps<{
  value: number
  max?: number
  label: string
  valueText?: string
  tone?: Tone
}>(), { max: 100, valueText: undefined, tone: 'info' })

const percent = computed(() => (props.max > 0 ? Math.min(100, Math.max(0, (props.value / props.max) * 100)) : 0))
const fill = computed(() => ({ '--ox-w': Number(percent.value.toFixed(1)) }))
</script>

<template lang="pug">
.ox-meter(
  role="meter"
  :class="`ox-tone-${tone}`"
  :aria-label="label"
  aria-valuemin="0"
  :aria-valuemax="max"
  :aria-valuenow="value"
  :aria-valuetext="valueText"
)
  span.ox-meter-fill(v-css-vars="fill")
</template>
