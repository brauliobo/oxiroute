<script setup vapor lang="ts">
import type { Metric } from './metric'
import './styles.css'

defineProps<{ items: readonly Metric[]; large?: boolean }>()
</script>

<template lang="pug">
dl.ox-metrics(:class="{ 'ox-metrics-large': large }")
  div(v-for="item in items" :key="item.label")
    dt {{ item.label }}
    dd(:class="{ 'ox-mono': item.mono }" :title="item.title") {{ item.value }}
</template>
