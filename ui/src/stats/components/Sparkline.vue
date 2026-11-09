<script setup vapor lang="ts">
import { computed } from 'vue'
import { formatCount } from '../format'
import { SPARK_HEIGHT, SPARK_WIDTH, sparkShape } from './spark'
import './widgets.css'

// A tiny trend of the last samples of a series; nothing is drawn until two samples are known.
const props = defineProps<{ series: ReadonlyArray<number | undefined>; label: string; format?: (value: number) => string }>()
const shape = computed(() => sparkShape(props.series))
const summary = computed(() => {
  const current = shape.value
  if (!current) return ''
  const format = props.format ?? formatCount
  const now = current.last === undefined ? 'unknown' : format(current.last)
  return `${props.label}: now ${now}, peak ${format(current.max)}, last ${current.known.length} samples`
})
const box = `0 0 ${SPARK_WIDTH} ${SPARK_HEIGHT}`
</script>

<template lang="pug">
svg.ox-spark(v-if="shape" role="img" focusable="false" :viewBox="box" :aria-label="summary")
  title {{ summary }}
  path(:d="shape.d")
</template>
